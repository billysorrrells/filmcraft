//! Adobe Premiere Pro project (`.prproj`) import.
//!
//! A `.prproj` is gzip-compressed XML (`<PremiereData Version="3">`): a flat table of objects that
//! point at each other by `ObjectID` / `ObjectRef` (numbers) and `ObjectUID` / `ObjectURef`
//! (GUIDs). Written clean-room from projects inspected as data; no Premiere code or SDK is used.
//!
//! What comes across:
//! - the bin tree (`RootProjectItem` → `BinProjectItem` → `ClipProjectItem`), with every file as a
//!   media item (path, frame size, rate, duration, audio rate),
//! - every sequence: frame size, frame rate, audio rate, video and audio tracks, and each clip's
//!   position, length and source in-point; nested sequences become nested sequence clips,
//! - track mute / lock, and audio–video linking for clips cut from the same source at the same
//!   place.
//!
//! Not yet: effects and their keyframes, transitions, markers, speed changes, titles and graphics,
//! multicam, adjustment layers. Anything skipped is reported as a warning, never silently dropped.
//!
//! Premiere keeps time in ticks of 254 016 000 000 per second, the same unit as [`Tick`], so times
//! come across exactly.

use std::collections::{HashMap, HashSet};
use std::io::Read;

use filmcraft_media::MediaKind;
use filmcraft_project::{BinId, ItemId, Sequence, TrackKind};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};
use roxmltree::{Document, Node};

use crate::common::{Builder, MediaSpec, empty_sequence, kind_from_ext, resolve_path, settings_for};
use crate::{Error, Format, ImportOptions, Imported, Report, Result};

/// Track-group media type GUIDs.
const VIDEO_GUID: &str = "228cda18-3625-4d2d-951e-348879e4ed93";
const AUDIO_GUID: &str = "80b8e3d5-6dca-4195-aefb-cb5f407ab009";

/// gzip magic.
pub(crate) fn is_gzip(bytes: &[u8]) -> bool {
    bytes.len() > 2 && bytes[0] == 0x1f && bytes[1] == 0x8b
}

/// Decompress a `.prproj` (plain XML is accepted as is).
pub(crate) fn decode(bytes: &[u8]) -> Result<String> {
    let raw = if is_gzip(bytes) {
        let mut out = Vec::new();
        flate2::read::MultiGzDecoder::new(bytes)
            .read_to_end(&mut out)
            .map_err(|e| Error::parse(Format::Prproj, format!("not a readable Premiere project (gzip): {e}")))?;
        out
    } else {
        bytes.to_vec()
    };
    let b = raw.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&raw);
    String::from_utf8(b.to_vec()).map_err(|_| Error::parse(Format::Prproj, "project XML is not UTF-8"))
}

/// Does this look like a Premiere project (after decompression)?
pub fn sniff(bytes: &[u8]) -> bool {
    match decode(bytes) {
        Ok(t) => t.get(..400).unwrap_or(&t).contains("<PremiereData"),
        Err(_) => false,
    }
}

pub(crate) fn import(bytes: &[u8], opts: &ImportOptions, report: &mut Report) -> Result<Imported> {
    let text = decode(bytes)?;
    let doc = Document::parse_with_options(&text, roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() })
        .map_err(|e| Error::parse(Format::Prproj, e.to_string()))?;
    let root = doc.root_element();
    if root.tag_name().name() != "PremiereData" {
        return Err(Error::parse(Format::Prproj, "root element is not <PremiereData>"));
    }
    let mut by_id: HashMap<&str, Node> = HashMap::new();
    let mut by_uid: HashMap<&str, Node> = HashMap::new();
    for n in root.children().filter(Node::is_element) {
        if let Some(id) = n.attribute("ObjectID") {
            by_id.insert(id, n);
        }
        if let Some(uid) = n.attribute("ObjectUID") {
            by_uid.insert(uid, n);
        }
    }
    let name = opts.name.clone().unwrap_or_else(|| "Premiere Project".into());
    let mut imp = Imp {
        b: Builder::new(&name),
        opts,
        report,
        by_id,
        by_uid,
        media_items: HashMap::new(),
        seq_items: HashMap::new(),
        in_progress: HashSet::new(),
        skipped: HashMap::new(),
    };
    let Some(root_item) = root.children().find(|n| n.has_tag_name("RootProjectItem")) else {
        return Err(Error::parse(Format::Prproj, "no RootProjectItem (project has no bins)"));
    };
    imp.walk_bin(root_item, None);
    // Sequences that exist but are not in any bin (rare) still come across.
    let orphans: Vec<Node> = root.children().filter(|n| n.has_tag_name("Sequence")).collect();
    for s in orphans {
        if let Some(uid) = s.attribute("ObjectUID")
            && !imp.seq_items.contains_key(uid)
            && let Some(id) = imp.sequence(s, None)
            && !imp.b.top.contains(&id)
        {
            imp.b.top.push(id);
        }
    }
    let mut skipped: Vec<_> = imp.skipped.drain().collect();
    skipped.sort();
    for (what, n) in skipped {
        imp.report.warn(format!("{n} {what} not imported yet"));
    }
    if imp.b.p.items.is_empty() {
        return Err(Error::Empty);
    }
    Ok(imp.b.finish())
}

struct Imp<'a, 'i, 'r> {
    b: Builder,
    opts: &'r ImportOptions,
    report: &'r mut Report,
    by_id: HashMap<&'a str, Node<'a, 'i>>,
    by_uid: HashMap<&'a str, Node<'a, 'i>>,
    /// Media item per `Media` ObjectUID.
    media_items: HashMap<String, ItemId>,
    /// Sequence item per `Sequence` ObjectUID.
    seq_items: HashMap<String, ItemId>,
    in_progress: HashSet<String>,
    skipped: HashMap<&'static str, usize>,
}

fn kid<'a, 'i>(n: Node<'a, 'i>, tag: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.has_tag_name(tag))
}

fn path<'a, 'i>(n: Node<'a, 'i>, tags: &[&str]) -> Option<Node<'a, 'i>> {
    tags.iter().try_fold(n, |n, t| kid(n, t))
}

fn text<'a>(n: Node<'a, '_>, tags: &[&str]) -> Option<&'a str> {
    path(n, tags).and_then(|n| n.text()).map(str::trim).filter(|s| !s.is_empty())
}

fn int(n: Node, tags: &[&str]) -> Option<i64> {
    text(n, tags).and_then(|s| s.parse().ok())
}

fn rect(n: Node, tags: &[&str]) -> Option<(u32, u32)> {
    let v: Vec<i64> = text(n, tags)?.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    let [x0, y0, x1, y1] = v.as_slice() else { return None };
    let (w, h) = (x1.checked_sub(*x0)?, y1.checked_sub(*y0)?);
    (w > 0 && h > 0).then(|| (u32::try_from(w).ok(), u32::try_from(h).ok())).and_then(|(w, h)| Some((w?, h?)))
}

/// Frame rate from a ticks-per-frame value.
fn rate_of(ticks_per_frame: i64) -> Option<FrameRate> {
    (ticks_per_frame > 0).then(|| FrameRate::new(TICKS_PER_SECOND, ticks_per_frame))
}

impl<'a, 'i> Imp<'a, 'i, '_> {
    /// Follow the `ObjectRef` / `ObjectURef` on `n`.
    fn deref(&self, n: Node<'a, 'i>) -> Option<Node<'a, 'i>> {
        if let Some(r) = n.attribute("ObjectRef") {
            return self.by_id.get(r).copied();
        }
        n.attribute("ObjectURef").and_then(|u| self.by_uid.get(u).copied())
    }

    fn deref_kid(&self, n: Node<'a, 'i>, tags: &[&str]) -> Option<Node<'a, 'i>> {
        path(n, tags).and_then(|r| self.deref(r))
    }

    fn skip(&mut self, what: &'static str) {
        *self.skipped.entry(what).or_default() += 1;
    }

    /// Walk a `RootProjectItem` / `BinProjectItem`'s items.
    fn walk_bin(&mut self, n: Node<'a, 'i>, bin: Option<BinId>) {
        let Some(items) = path(n, &["ProjectItemContainer", "Items"]) else { return };
        for it in items.children().filter(|c| c.has_tag_name("Item")) {
            let Some(obj) = self.deref(it) else { continue };
            let name = text(obj, &["ProjectItem", "Name"]).unwrap_or("Item").to_string();
            match obj.tag_name().name() {
                "BinProjectItem" => {
                    let b = self.b.bin(&name, bin);
                    self.walk_bin(obj, Some(b));
                }
                "ClipProjectItem" => self.project_clip(obj, &name, bin),
                // Search bins and other project item kinds have no media of their own.
                _ => self.skip("other project items (search bins, team items)"),
            }
        }
    }

    /// A `ClipProjectItem`: a media file or a sequence.
    fn project_clip(&mut self, n: Node<'a, 'i>, name: &str, bin: Option<BinId>) {
        let Some(master) = self.deref_kid(n, &["MasterClip"]) else { return };
        let Some(clips) = kid(master, "Clips") else { return };
        // The first clip whose source resolves decides what the item is.
        for c in clips.children().filter(|c| c.has_tag_name("Clip")) {
            let Some(clip) = self.deref(c) else { continue };
            let Some(src) = self.deref_kid(clip, &["Clip", "Source"]) else { continue };
            match src.tag_name().name() {
                "VideoSequenceSource" | "AudioSequenceSource" => {
                    if let Some(seq) = self.deref_kid(src, &["SequenceSource", "Sequence"])
                        && let Some(id) = self.sequence_named(seq, Some(name), bin)
                        && !self.b.top.contains(&id)
                    {
                        self.b.top.push(id);
                    }
                    return;
                }
                "VideoMediaSource" | "AudioMediaSource" => {
                    if let Some(media) = self.deref_kid(src, &["MediaSource", "Media"]) {
                        self.media_item(media, Some(name), bin);
                    }
                    return;
                }
                _ => {}
            }
        }
        self.skip("clips with no file (titles, generators, synthetic media)");
    }

    /// Media item for a `Media` object.
    fn media_item(&mut self, media: Node<'a, 'i>, name: Option<&str>, bin: Option<BinId>) -> Option<ItemId> {
        let uid = media.attribute("ObjectUID")?.to_string();
        if let Some(id) = self.media_items.get(&uid) {
            return Some(*id);
        }
        let raw = text(media, &["ActualMediaFilePath"]).or_else(|| text(media, &["FilePath"]))?;
        let file_name = raw.rsplit(['\\', '/']).next().unwrap_or(raw);
        let name = name.or_else(|| text(media, &["Title"])).unwrap_or(file_name).to_string();
        let mut p = resolve_path(raw, self.opts.base_dir.as_deref());
        // A project moved to another machine: fall back to the copy next to the project
        // (RelativePath), as Premiere's own relink does.
        if !std::path::Path::new(&p).exists()
            && let Some(rel) = text(media, &["RelativePath"])
            && let Some(base) = self.opts.base_dir.as_deref()
        {
            let cand = resolve_path(&rel.replace('\\', "/"), Some(base));
            if std::path::Path::new(&cand).exists() {
                p = cand;
            }
        }
        let vs = self.deref_kid(media, &["VideoStream"]);
        let aus = self.deref_kid(media, &["AudioStream"]);
        let video = vs.map(|v| {
            let (w, h) = rect(v, &["FrameRect"]).unwrap_or((1920, 1080));
            (w, h, int(v, &["FrameRate"]).and_then(rate_of).unwrap_or_default())
        });
        let audio = aus.map(|a| {
            let sr = int(a, &["FrameRate"]).filter(|t| *t > 0).and_then(|t| u32::try_from(TICKS_PER_SECOND / t).ok()).unwrap_or(48_000);
            let ch = text(a, &["AudioChannelLayout"]).map(|l| l.matches("channellabel").count() as u32).filter(|c| *c > 0).unwrap_or(2);
            (sr, ch)
        });
        let duration = vs.and_then(|v| int(v, &["Duration"])).or_else(|| aus.and_then(|a| int(a, &["Duration"]))).filter(|d| *d > 0).map(Tick);
        let mut spec = MediaSpec { duration, video, audio, start_tc: None, kind: None };
        if spec.video.is_none() && spec.audio.is_none() {
            match kind_from_ext(&p) {
                MediaKind::AudioOnly => spec.audio = Some((48_000, 2)),
                k => {
                    spec.video = Some((1920, 1080, FrameRate::default()));
                    spec.kind = Some(k);
                }
            }
        }
        let id = self.b.file_media(&format!("file:{p}"), &name, &p, &spec, bin);
        self.media_items.insert(uid, id);
        Some(id)
    }

    fn sequence(&mut self, n: Node<'a, 'i>, bin: Option<BinId>) -> Option<ItemId> {
        self.sequence_named(n, None, bin)
    }

    /// Sequence item for a `Sequence` object (built once; nested uses share it).
    fn sequence_named(&mut self, n: Node<'a, 'i>, item_name: Option<&str>, bin: Option<BinId>) -> Option<ItemId> {
        let uid = n.attribute("ObjectUID")?.to_string();
        if let Some(id) = self.seq_items.get(&uid) {
            return Some(*id);
        }
        if !self.in_progress.insert(uid.clone()) {
            self.report.warn("recursive nested sequence ignored");
            return None;
        }
        let name = item_name.or_else(|| text(n, &["Name"])).unwrap_or("Sequence").to_string();
        // Track groups: (media type GUID, group object).
        let mut groups: Vec<(&str, Node<'a, 'i>)> = Vec::new();
        if let Some(tg) = kid(n, "TrackGroups") {
            for g in tg.children().filter(|c| c.has_tag_name("TrackGroup")) {
                if let (Some(kind), Some(obj)) = (text(g, &["First"]), self.deref_kid(g, &["Second"])) {
                    groups.push((kind, obj));
                }
            }
        }
        let vgroup = groups.iter().find(|(k, _)| *k == VIDEO_GUID).map(|g| g.1);
        let agroup = groups.iter().find(|(k, _)| *k == AUDIO_GUID).map(|g| g.1);
        let rate = vgroup.and_then(|g| int(g, &["TrackGroup", "FrameRate"])).and_then(rate_of).unwrap_or_default();
        let (w, h) = vgroup.and_then(|g| rect(g, &["FrameRect"])).unwrap_or((1920, 1080));
        let mut settings = settings_for(rate, w, h, false);
        if let Some(sr) = agroup.and_then(|g| int(g, &["TrackGroup", "FrameRate"])).filter(|t| *t > 0) {
            settings.sample_rate = u32::try_from(TICKS_PER_SECOND / sr).unwrap_or(48_000);
        }
        let id = self.b.reserve_sequence(&name, settings.clone(), bin);
        self.seq_items.insert(uid.clone(), id);
        let mut seq = empty_sequence(settings);
        // (kind, track, start, end, master clip uid) per placed clip, for linking.
        let mut placed: Vec<(TrackKind, usize, Tick, Tick, String, filmcraft_project::ClipId)> = Vec::new();
        for (kind, group) in [(TrackKind::Video, vgroup), (TrackKind::Audio, agroup)] {
            let Some(group) = group else { continue };
            let Some(tracks) = path(group, &["TrackGroup", "Tracks"]) else { continue };
            for (ti, tref) in tracks.children().filter(|c| c.has_tag_name("Track")).enumerate() {
                self.b.ensure_tracks(&mut seq, kind, ti + 1);
                let Some(track) = self.deref(tref) else { continue };
                let tnode = path(track, &["ClipTrack", "Track"]);
                if let Some(t) = tnode
                    && let Some(st) = seq.tracks_mut(kind).get_mut(ti)
                {
                    if kind == TrackKind::Audio {
                        st.muted = text(t, &["IsMuted"]) == Some("true");
                    }
                    st.locked = text(t, &["IsLocked"]) == Some("true");
                }
                if path(track, &["ClipTrack", "TransitionItems", "TrackItems"]).is_some_and(|t| t.children().any(|c| c.is_element())) {
                    self.skip("transitions");
                }
                let Some(items) = path(track, &["ClipTrack", "ClipItems", "TrackItems"]) else { continue };
                for iref in items.children().filter(|c| c.has_tag_name("TrackItem")) {
                    let Some(ti_node) = self.deref(iref) else { continue };
                    if let Some(p) = self.track_item(ti_node, kind, &mut seq, ti) {
                        placed.push(p);
                    }
                }
            }
        }
        // Link video and audio cut from the same master clip at the same place.
        let mut groups_of: HashMap<(String, i64, i64), Vec<(TrackKind, filmcraft_project::ClipId)>> = HashMap::new();
        for (k, _, s, e, m, cid) in &placed {
            if !m.is_empty() {
                groups_of.entry((m.clone(), s.0, e.0)).or_default().push((*k, *cid));
            }
        }
        // Only video with its own audio: stacked copies of one clip on two video tracks stay apart.
        for members in groups_of.into_values() {
            let v = members.iter().filter(|(k, _)| *k == TrackKind::Video).count();
            let a = members.len() - v;
            if v > 1 || v == 0 || a == 0 {
                continue;
            }
            let members: Vec<_> = members.into_iter().map(|(_, c)| c).collect();
            let g = self.b.link_id();
            for cid in members {
                if let Some((_, it)) = seq.find_item_mut(cid) {
                    it.link = Some(g);
                }
            }
        }
        self.b.put_sequence(id, seq);
        self.in_progress.remove(&uid);
        Some(id)
    }

    /// One clip on a track. Returns what linking needs.
    fn track_item(
        &mut self,
        n: Node<'a, 'i>,
        kind: TrackKind,
        seq: &mut Sequence,
        track: usize,
    ) -> Option<(TrackKind, usize, Tick, Tick, String, filmcraft_project::ClipId)> {
        let cti = kid(n, "ClipTrackItem")?;
        let start = Tick(int(cti, &["TrackItem", "Start"])?);
        let end = Tick(int(cti, &["TrackItem", "End"])?);
        if end <= start {
            return None;
        }
        let sub = self.deref_kid(cti, &["SubClip"])?;
        let name = text(sub, &["Name"]).unwrap_or("Clip").to_string();
        let master_uid = path(sub, &["MasterClip"]).and_then(|m| m.attribute("ObjectURef")).unwrap_or("").to_string();
        let clip = self.deref_kid(sub, &["Clip"])?;
        let c = kid(clip, "Clip")?;
        let source_in = Tick(int(c, &["InPoint"]).unwrap_or(0));
        if text(c, &["PlaybackSpeed"]).and_then(|s| s.parse::<f64>().ok()).is_some_and(|s| (s - 1.0).abs() > 1e-6) {
            self.skip("speed changes (clips placed at 100%)");
        }
        let src = self.deref_kid(c, &["Source"])?;
        let item = match src.tag_name().name() {
            "VideoMediaSource" | "AudioMediaSource" => {
                let media = self.deref_kid(src, &["MediaSource", "Media"])?;
                self.media_item(media, None, None)?
            }
            "VideoSequenceSource" | "AudioSequenceSource" => {
                let s = self.deref_kid(src, &["SequenceSource", "Sequence"])?;
                self.sequence(s, None)?
            }
            _ => {
                self.skip("graphics, titles and generated clips");
                return None;
            }
        };
        let mut it = self.b.clip(item, kind, &name, start, end - start, source_in);
        if text(cti, &["TrackItem", "Disabled"]) == Some("true") || text(n, &["Disabled"]) == Some("true") {
            it.enabled = false;
        }
        let cid = it.id;
        seq.tracks_mut(kind).get_mut(track)?.items.push(it);
        Some((kind, track, start, end, master_uid, cid))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINI: &str = r#"<?xml version="1.0" encoding="UTF-8" ?>
<PremiereData Version="3">
  <RootProjectItem ObjectUID="root"><ProjectItem><Name>Root Bin</Name></ProjectItem>
    <ProjectItemContainer><Items><Item Index="0" ObjectURef="bin1"/><Item Index="1" ObjectURef="seqitem"/></Items></ProjectItemContainer>
  </RootProjectItem>
  <BinProjectItem ObjectUID="bin1"><ProjectItem><Name>Footage</Name></ProjectItem>
    <ProjectItemContainer><Items><Item Index="0" ObjectURef="clipitem"/></Items></ProjectItemContainer>
  </BinProjectItem>
  <ClipProjectItem ObjectUID="clipitem"><ProjectItem><Name>Take 1</Name></ProjectItem><MasterClip ObjectURef="mc1"/></ClipProjectItem>
  <MasterClip ObjectUID="mc1"><Clips><Clip Index="0" ObjectRef="10"/></Clips></MasterClip>
  <VideoClip ObjectID="10"><Clip><Source ObjectRef="11"/><InPoint>0</InPoint></Clip></VideoClip>
  <VideoMediaSource ObjectID="11"><MediaSource><Media ObjectURef="m1"/></MediaSource></VideoMediaSource>
  <Media ObjectUID="m1"><VideoStream ObjectRef="12"/><FilePath>/footage/take1.mov</FilePath><Title>take1.mov</Title></Media>
  <VideoStream ObjectID="12"><FrameRate>10584000000</FrameRate><FrameRect>0,0,1280,720</FrameRect><Duration>2540160000000</Duration></VideoStream>
  <ClipProjectItem ObjectUID="seqitem"><ProjectItem><Name>Edit 1</Name></ProjectItem><MasterClip ObjectURef="mc2"/></ClipProjectItem>
  <MasterClip ObjectUID="mc2"><Clips><Clip Index="0" ObjectRef="20"/></Clips></MasterClip>
  <VideoClip ObjectID="20"><Clip><Source ObjectRef="21"/></Clip></VideoClip>
  <VideoSequenceSource ObjectID="21"><SequenceSource><Sequence ObjectURef="s1"/></SequenceSource></VideoSequenceSource>
  <Sequence ObjectUID="s1"><TrackGroups>
    <TrackGroup Index="0"><First>228cda18-3625-4d2d-951e-348879e4ed93</First><Second ObjectRef="30"/></TrackGroup>
  </TrackGroups><Name>Edit 1</Name></Sequence>
  <VideoTrackGroup ObjectID="30"><TrackGroup><Tracks><Track Index="0" ObjectURef="t1"/></Tracks><FrameRate>10584000000</FrameRate></TrackGroup><FrameRect>0,0,1280,720</FrameRect></VideoTrackGroup>
  <VideoClipTrack ObjectUID="t1"><ClipTrack><Track><IsLocked>false</IsLocked></Track><ClipItems><TrackItems><TrackItem Index="0" ObjectRef="40"/></TrackItems></ClipItems></ClipTrack></VideoClipTrack>
  <VideoClipTrackItem ObjectID="40"><ClipTrackItem><TrackItem><Start>254016000000</Start><End>762048000000</End></TrackItem><SubClip ObjectRef="41"/></ClipTrackItem></VideoClipTrackItem>
  <SubClip ObjectID="41"><Clip ObjectRef="42"/><MasterClip ObjectURef="mc1"/><Name>Take 1</Name></SubClip>
  <VideoClip ObjectID="42"><Clip><Source ObjectRef="11"/><InPoint>508032000000</InPoint></Clip></VideoClip>
</PremiereData>"#;

    fn gz(s: &str) -> Vec<u8> {
        use std::io::Write;
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(s.as_bytes()).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn imports_bins_media_and_a_sequence() {
        let mut r = Report::default();
        let imp = import(&gz(MINI), &ImportOptions::default(), &mut r).unwrap();
        assert_eq!(imp.sequences.len(), 1);
        let p = &imp.project;
        let seq = p.sequence(imp.sequences[0]).unwrap();
        assert_eq!(seq.settings.frame_rate, FrameRate::new(24, 1));
        assert_eq!((seq.settings.width, seq.settings.height), (1280, 720));
        let v = &seq.tracks(TrackKind::Video)[0].items;
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].start, Tick(TICKS_PER_SECOND));
        assert_eq!(v[0].duration, Tick(2 * TICKS_PER_SECOND));
        assert_eq!(v[0].source_in, Tick(2 * TICKS_PER_SECOND));
        assert!(p.root.children.iter().any(|c| matches!(c, filmcraft_project::BinEntry::Bin(b) if b.name == "Footage")));
    }

    #[test]
    fn plain_xml_is_accepted_and_sniffed() {
        assert!(sniff(MINI.as_bytes()));
        assert!(sniff(&gz(MINI)));
        assert!(!sniff(b"<xmeml version=\"5\"/>"));
        let mut r = Report::default();
        assert!(import(MINI.as_bytes(), &ImportOptions::default(), &mut r).is_ok());
    }
}
