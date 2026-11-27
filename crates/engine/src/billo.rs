//! Billo Cut commands (`billo.*`): the Billo Zord / Render Wall workflow on top of FilmCraft.
//!
//! Every command here is built from ordinary FilmCraft commands and clip data, so what it does
//! undoes, saves, exports and renders like any other edit.
//!
//! | command | does |
//! |---|---|
//! | `billo.shot.setStatus` | marks shots locked / real / regen / review: clip label colour plus a status marker |
//! | `billo.shot.lane` | the status lane: every picture clip in time order with its status, for the Zord and the Wall |
//! | `billo.stems.lay` | lays separated stems (vocals, drums, bass, other …) under a song clip, each on its own named track, in sync |
//! | `billo.stems.solo` | solos one stem track (or clears solo) so it can be worked against picture |
//! | `billo.dialogue.clean` | Dialogue type + "Clean Up Noisy Dialogue" + loudness match, in one step |
//! | `billo.deliver.loudness` | auto-matches every typed audio clip to a delivery target (default −14 LUFS) |
//! | `billo.wall.package` | writes the Render Wall package folders (04_INDEX, 05_TIMELINE with OTIO / FCPXML / EDL, README) |
//! | `billo.fxVault.list` | the Billo FX Vault catalogue, filterable by category and status |
//! | `billo.stems.split` | splits a song clip into Vocals / Drums / Bass / Other inside the editor (quick, model-free) and lays them |
//! | `billo.shot.swapTake` | swaps another take into a shot's slot and flags it for review |
//! | `billo.version.save` / `.list` / `.payload` | keeps numbered copies of the cut; `.payload` is the row Billo OS stores |
//! | `billo.regen.request` | flags shots for a regen and returns the generator ticket (480p drafts) |
//! | `billo.wall.open` / `billo.wall.timeline` | opens a Render Wall (Billo Zord) timeline as a sequence, and writes the cut back in that schema |
//! | `billo.upscale.request` | queues shots (or the whole cut) for the upscale runner and returns the job rows |
//! | `billo.beats.detect` / `billo.beats.snapCuts` | finds the kicks in a song, marks them, and rolls the picture cuts onto them |
//! | `billo.cut.silence` | cuts the dead air out of a talking clip and closes the cut up |
//! | `billo.assistant.run` / `.catalog` | plain-language requests routed to commands from a fixed list (no model) |

use serde_json::{Value, json};

use filmcraft_project::{ClipId, Label};

use crate::commands::{CommandSpec, always, bad, has_seq, str_p};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

/// Most stems one `billo.stems.lay` takes.
const MAX_STEMS: usize = 8;
/// The FX Vault catalogue shipped with the app (mirrors billotech-fx-vault).
const FX_VAULT: &str = include_str!("billo_fx_vault.json");

/// Shot states the Zord status lane knows, and the clip label each one paints.
const STATUSES: [(&str, Label); 4] = [("locked", Label::Forest), ("real", Label::Blue), ("regen", Label::Rose), ("review", Label::Yellow)];

fn status_label(name: &str) -> Option<Label> {
    let k = name.trim().to_ascii_lowercase();
    STATUSES.iter().find(|(n, _)| *n == k).map(|(_, l)| *l)
}

/// The shot status a clip label stands for, if any.
pub fn label_status(l: Label) -> Option<&'static str> {
    STATUSES.iter().find(|(_, x)| *x == l).map(|(n, _)| *n)
}

/// `clips` (or `clip`) from the params, else the selection.
fn clip_targets(s: &Session, p: &Value) -> Vec<ClipId> {
    match p.get("clips").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(ClipId)).collect(),
        None => match p.get("clip").and_then(Value::as_u64) {
            Some(c) => vec![ClipId(c)],
            None => s.state.selection.clone(),
        },
    }
}

fn set_status(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "billo.shot.setStatus";
    let status = str_p(p, "status").ok_or_else(|| bad(cmd, "need `status`: locked|real|regen|review"))?.to_string();
    let label = status_label(&status).ok_or_else(|| bad(cmd, "`status` must be locked, real, regen or review"))?;
    let note = str_p(p, "note").map(str::to_string);
    let clips = clip_targets(s, p);
    if clips.is_empty() {
        return Err(bad(cmd, "select the shots (or pass `clips`)"));
    }
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let mut starts = Vec::new();
    {
        let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
        for t in seq.video_tracks.iter().chain(seq.audio_tracks.iter()) {
            for it in t.items.iter().filter(|i| clips.contains(&i.id)) {
                starts.push((it.id, it.start, it.name.clone()));
            }
        }
    }
    if starts.is_empty() {
        return Err(bad(cmd, "none of those clips are in the active sequence"));
    }
    let ids: Vec<ClipId> = starts.iter().map(|(c, _, _)| *c).collect();
    s.edit("Set Shot Status", |pr, _| {
        let seq = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        for c in &ids {
            if let Some((_, it)) = seq.find_item_mut(*c) {
                it.label = label;
            }
        }
        Ok(())
    })?;
    // One status marker per shot start (picture clips only once even when linked).
    let mut marked: Vec<i64> = Vec::new();
    for (_, at, name) in &starts {
        if marked.contains(&at.0) {
            continue;
        }
        marked.push(at.0);
        let title = match &note {
            Some(n) if !n.is_empty() => format!("{}: {n}", status.to_ascii_uppercase()),
            _ => format!("{}: {name}", status.to_ascii_uppercase()),
        };
        s.execute("markers.add", json!({"time": at.0, "name": title, "color": label.name()}))?;
    }
    Ok(json!({"status": status, "label": label.name(), "clips": ids.iter().map(|c| c.0).collect::<Vec<_>>()}))
}

fn lane(s: &mut Session, _p: &Value) -> Result<Value> {
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let mut shots: Vec<Value> = Vec::new();
    let mut counts = json!({"locked": 0, "real": 0, "regen": 0, "review": 0, "unmarked": 0});
    for (ti, t) in seq.video_tracks.iter().enumerate() {
        for it in &t.items {
            let st = label_status(it.label).unwrap_or("unmarked");
            if let Some(n) = counts.get_mut(st).and_then(|v| v.as_u64()) {
                counts[st] = json!(n.saturating_add(1));
            }
            shots.push(json!({
                "clip": it.id.0, "name": it.name, "track": format!("V{}", ti.saturating_add(1)),
                "start": it.start.0, "seconds": it.start.seconds(), "durationSeconds": it.duration.seconds(),
                "status": st, "label": it.label.name(),
            }));
        }
    }
    shots.sort_by_key(|v| v["start"].as_i64().unwrap_or(0));
    let total = shots.len();
    let done = counts["locked"].as_u64().unwrap_or(0) as usize;
    Ok(json!({"shots": shots, "counts": counts, "total": total, "lockedPercent": done.saturating_mul(100).checked_div(total).unwrap_or(0)}))
}

fn stems_lay(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "billo.stems.lay";
    let song = p.get("song").and_then(Value::as_u64).map(ClipId).ok_or_else(|| bad(cmd, "need `song`: the song's audio clip id"))?;
    let stems = p.get("stems").and_then(Value::as_array).ok_or_else(|| bad(cmd, "need `stems`: [{item, name}]"))?.clone();
    if stems.is_empty() || stems.len() > MAX_STEMS {
        return Err(bad(cmd, format!("`stems` needs 1 to {MAX_STEMS} entries")));
    }
    let (start, source_in, duration) = {
        let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let it = seq
            .audio_tracks
            .iter()
            .flat_map(|t| t.items.iter())
            .find(|i| i.id == song)
            .ok_or_else(|| bad(cmd, "`song` is not an audio clip in the active sequence"))?;
        let source_in = if p.get("fromClipStart").and_then(Value::as_bool).unwrap_or(false) { filmcraft_time::Tick::ZERO } else { it.source_in };
        (it.start, source_in, it.duration)
    };
    let mut laid = Vec::new();
    for st in &stems {
        let item = st.get("item").and_then(Value::as_u64).ok_or_else(|| bad(cmd, "each stem needs `item` (a project item id)"))?;
        let name = st.get("name").and_then(Value::as_str).unwrap_or("Stem").chars().take(40).collect::<String>();
        s.execute(
            "timeline.place",
            json!({"item": item, "audioTrack": "new", "time": start.0, "sourceIn": source_in.0, "duration": duration.0, "video": false, "audio": true}),
        )?;
        let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let track = seq.audio_tracks.last().map(|t| t.id);
        let Some(tid) = track else { return Err(bad(cmd, "stem track was not created")) };
        s.execute("timeline.setTrack", json!({"track": tid.0, "name": name}))?;
        laid.push(json!({"item": item, "name": name, "track": tid.0}));
    }
    if p.get("muteSong").and_then(Value::as_bool).unwrap_or(false) {
        let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
        s.edit("Mute Song Under Stems", |pr, _| {
            let seq = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
            if let Some((_, it)) = seq.find_item_mut(song) {
                it.enabled = false;
            }
            Ok(())
        })?;
    }
    Ok(json!({"song": song.0, "stems": laid}))
}

fn stems_solo(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "billo.stems.solo";
    let want = str_p(p, "name").map(str::to_ascii_lowercase);
    let tracks: Vec<(u64, String)> = {
        let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
        seq.audio_tracks.iter().map(|t| (t.id.0, t.name.clone())).collect()
    };
    let mut hit = None;
    for (id, name) in &tracks {
        let solo = want.as_deref().is_some_and(|w| name.to_ascii_lowercase() == w);
        if solo {
            hit = Some(name.clone());
        }
        s.execute("timeline.setTrack", json!({"track": id, "solo": solo}))?;
    }
    if want.is_some() && hit.is_none() {
        return Err(bad(cmd, "no audio track with that name"));
    }
    Ok(json!({"solo": hit}))
}

fn audio_clip_ids(s: &Session) -> Vec<u64> {
    s.active_sequence().map(|q| q.audio_tracks.iter().flat_map(|t| t.items.iter()).map(|i| i.id.0).collect()).unwrap_or_default()
}

fn dialogue_clean(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "billo.dialogue.clean";
    let clips: Vec<u64> = match p.get("clips").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(Value::as_u64).collect(),
        None => s.state.selection.iter().map(|c| c.0).collect(),
    };
    if clips.is_empty() {
        return Err(bad(cmd, "select the dialogue clips (or pass `clips`)"));
    }
    let target = p.get("target").and_then(Value::as_f64).unwrap_or(-16.0).clamp(-40.0, -5.0);
    s.execute("essentialSound.setType", json!({"clips": clips, "type": "dialogue"}))?;
    s.execute("essentialSound.applyPreset", json!({"clips": clips, "preset": "Clean Up Noisy Dialogue", "type": "dialogue"}))?;
    let matched = s.execute("essentialSound.autoMatch", json!({"clips": clips, "target": target}))?;
    Ok(json!({"clips": clips, "target": target, "match": matched}))
}

fn deliver_loudness(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "billo.deliver.loudness";
    let target = p.get("target").and_then(Value::as_f64).unwrap_or(-14.0).clamp(-40.0, -5.0);
    let typed: Vec<u64> = s
        .active_sequence()
        .map(|q| q.audio_tracks.iter().flat_map(|t| t.items.iter()).filter(|i| i.essential.is_some()).map(|i| i.id.0).collect())
        .unwrap_or_default();
    if typed.is_empty() {
        let all = audio_clip_ids(s);
        if all.is_empty() {
            return Err(bad(cmd, "no audio clips in the sequence"));
        }
        return Err(bad(cmd, "give the clips an audio type first (billo.dialogue.clean or essentialSound.setType)"));
    }
    let r = s.execute("essentialSound.autoMatch", json!({"clips": typed, "target": target}))?;
    Ok(json!({"target": target, "clips": typed, "match": r}))
}

fn wall_package(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "billo.wall.package";
    let dir = str_p(p, "dir").ok_or_else(|| bad(cmd, "need `dir`: the package folder"))?.trim_end_matches('/').to_string();
    let name = {
        let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
        let raw = s.project.item(seq_id).map(|i| i.name.clone()).unwrap_or_default();
        let n: String = raw.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        if n.is_empty() { "timeline".to_string() } else { n }
    };
    let folders = ["01_MEDIA", "02_AUDIO", "03_TRANSCRIPT", "04_INDEX", "05_TIMELINE", "06_RENDERS", "07_QC"];
    if !cfg!(target_arch = "wasm32") {
        for f in folders {
            std::fs::create_dir_all(format!("{dir}/{f}")).map_err(|e| bad(cmd, format!("{dir}/{f}: {e}")))?;
        }
    }
    let mut written = Vec::new();
    for (id, ext) in [("file.exportOtio", "otio"), ("file.exportFcpxml", "fcpxml"), ("file.exportEdl", "edl")] {
        let path = format!("{dir}/05_TIMELINE/{name}.{ext}");
        match s.execute(id, json!({"path": path})) {
            Ok(_) => written.push(path),
            Err(e) => written.push(format!("{path} (skipped: {e})")),
        }
    }
    let lane = lane(s, &json!({}))?;
    let idx = format!("{dir}/04_INDEX/shots.json");
    let body = serde_json::to_string_pretty(&lane).map_err(|e| bad(cmd, e.to_string()))?;
    s.services.write_file(&idx, body.as_bytes()).map_err(|e| bad(cmd, format!("{idx}: {e}")))?;
    written.push(idx);
    let readme = format!(
        "# {name}\n\nBillo Cut Render Wall package.\n\n- 01_MEDIA source media\n- 02_AUDIO stems\n- 03_TRANSCRIPT transcript and captions\n- 04_INDEX shots.json (status lane: {} shots, {}% locked)\n- 05_TIMELINE OTIO / FCPXML / EDL\n- 06_RENDERS renders\n- 07_QC teardown\n",
        lane["total"], lane["lockedPercent"]
    );
    let rp = format!("{dir}/README.md");
    s.services.write_file(&rp, readme.as_bytes()).map_err(|e| bad(cmd, format!("{rp}: {e}")))?;
    written.push(rp);
    Ok(json!({"dir": dir, "files": written}))
}

/// The FX Vault catalogue (parsed once).
pub fn fx_vault() -> &'static [Value] {
    static ITEMS: std::sync::OnceLock<Vec<Value>> = std::sync::OnceLock::new();
    ITEMS.get_or_init(|| serde_json::from_str(FX_VAULT).unwrap_or_default())
}

fn fx_vault_list(_s: &mut Session, p: &Value) -> Result<Value> {
    let all: Vec<Value> = fx_vault().to_vec();
    let cat = str_p(p, "category").map(str::to_ascii_lowercase);
    let st = str_p(p, "status").map(str::to_ascii_lowercase);
    let items: Vec<Value> = all
        .into_iter()
        .filter(|v| cat.as_deref().is_none_or(|c| v["category"].as_str() == Some(c)))
        .filter(|v| st.as_deref().is_none_or(|x| v["status"].as_str() == Some(x)))
        .collect();
    Ok(json!({"count": items.len(), "items": items}))
}

// ------------------------------------------------------------------------------------- stems

/// The song clip a stem command works on: `song`, else the first selected audio clip.
fn song_target(s: &Session, p: &Value, cmd: &str) -> Result<ClipId> {
    if let Some(c) = p.get("song").and_then(Value::as_u64) {
        return Ok(ClipId(c));
    }
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    seq.audio_tracks
        .iter()
        .flat_map(|t| t.items.iter())
        .find(|i| s.state.selection.contains(&i.id))
        .map(|i| i.id)
        .ok_or_else(|| bad(cmd, "select the song's audio clip (or pass `song`)"))
}

/// Split a song clip into Vocals / Drums / Bass / Other inside the editor (the quick, model-free
/// split of [`filmcraft_audio_dsp::stems`]), write the four files, and lay them under the song.
fn stems_split(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "billo.stems.split";
    let song = song_target(s, p, cmd)?;
    let (left, right, sr, name) = {
        let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let sr = seq.settings.sample_rate.max(1);
        let it = seq
            .audio_tracks
            .iter()
            .flat_map(|t| t.items.iter())
            .find(|i| i.id == song)
            .ok_or_else(|| bad(cmd, "`song` is not an audio clip in the active sequence"))?;
        let a0 = it.start.to_units_floor(sr as i64);
        let a1 = it.end().to_units_floor(sr as i64);
        let total = usize::try_from(a1.saturating_sub(a0)).unwrap_or(0);
        if total == 0 {
            return Err(bad(cmd, "the clip is empty"));
        }
        if total > filmcraft_audio_dsp::stems::MAX_SAMPLES {
            return Err(bad(cmd, "the clip is longer than 20 minutes; cut it down first"));
        }
        let provider = s.media.provider(s.project.clone(), s.services.clone());
        let (mut left, mut right) = (Vec::new(), Vec::new());
        left.try_reserve(total).map_err(|e| bad(cmd, format!("not enough memory: {e}")))?;
        right.try_reserve(total).map_err(|e| bad(cmd, format!("not enough memory: {e}")))?;
        let mut pos = a0;
        while pos < a1 {
            let n = usize::try_from((a1 - pos).min(sr as i64)).unwrap_or(0).max(1);
            let [l, r] = filmcraft_render::audio::clip_signal(it, pos, n, sr, &provider).ok_or_else(|| bad(cmd, "the clip has no audio to split"))?;
            left.extend_from_slice(&l);
            right.extend_from_slice(&r);
            pos = pos.saturating_add(n as i64);
        }
        let name: String = it.name.rsplit_once('.').map_or(it.name.as_str(), |(base, _)| base).chars().take(60).collect();
        (left, right, sr, name)
    };
    let stems = filmcraft_audio_dsp::stems::split_stereo(&left, &right, sr).ok_or_else(|| bad(cmd, "nothing to split"))?;
    drop((left, right));
    let dir = crate::voiceover::media_dir(s, p, "Billo Stems");
    if !cfg!(target_arch = "wasm32") {
        std::fs::create_dir_all(&dir).map_err(|e| EngineError::Other(format!("{dir}: {e}")))?;
    }
    let n0 = s.history.undo.len();
    let mut laid = Vec::new();
    for (stem, label) in stems.into_iter().zip(filmcraft_audio_dsp::stems::STEM_NAMES) {
        let path = crate::voiceover::unique_wav_path(s, &dir, &format!("{name} {label}"))?;
        let bytes = crate::previews::write_wav_f32(&stem, sr);
        drop(stem);
        s.services.write_file(&path, &bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
        let item = crate::commands::import_bytes(s, &path, bytes.into(), None)?;
        laid.push(json!({"item": item.0, "name": label, "path": path}));
    }
    let mute = p.get("muteSong").and_then(Value::as_bool).unwrap_or(true);
    let r = stems_lay(s, &json!({"song": song.0, "stems": laid, "muteSong": mute, "fromClipStart": true}));
    crate::clip_ops::collapse_history(s, n0, "Split Stems");
    let r = r?;
    Ok(json!({"song": song.0, "method": "quick", "sampleRate": sr, "stems": r["stems"], "files": laid}))
}

// ------------------------------------------------------------------------------------- takes, versions, regen

/// Swap another take (a project item) into a shot's slot, keeping its place and length, and flag
/// the shot for review.
fn swap_take(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "billo.shot.swapTake";
    let clip = p
        .get("clip")
        .and_then(Value::as_u64)
        .map(ClipId)
        .or_else(|| s.state.selection.first().copied())
        .ok_or_else(|| bad(cmd, "select the shot (or pass `clip`)"))?;
    let item = p
        .get("item")
        .and_then(Value::as_u64)
        .map(filmcraft_project::ItemId)
        .or_else(|| s.state.project_selection.first().copied())
        .ok_or_else(|| bad(cmd, "pick the take in the Project panel (or pass `item`)"))?;
    let n0 = s.history.undo.len();
    s.state.selection = vec![clip];
    s.state.project_selection = vec![item];
    let r = s
        .execute("clip.replaceFromBin", json!({"clips": [clip.0], "item": item.0}))
        .and_then(|_| set_status(s, &json!({"clips": [clip.0], "status": "review", "note": "new take"})));
    crate::clip_ops::collapse_history(s, n0, "Swap Take");
    r?;
    Ok(json!({"clip": clip.0, "item": item.0, "status": "review"}))
}

fn sequence_name(s: &Session) -> Result<(filmcraft_project::ItemId, String)> {
    let id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let name = s.project.item(id).map(|i| i.name.clone()).unwrap_or_default();
    Ok((id, name))
}

/// The name a version is filed under: the part before ` · v<N>`.
pub fn version_base(name: &str) -> &str {
    name.rsplit_once(" · v").filter(|(_, tail)| tail.chars().next().is_some_and(|c| c.is_ascii_digit())).map_or(name, |(base, _)| base)
}

/// Saved versions of the cut named `base`: (sequence id, name, number), oldest first.
pub fn versions_of(s: &Session, base: &str) -> Vec<(u64, String, u32)> {
    let prefix = format!("{base} · v");
    let mut v: Vec<(u64, String, u32)> = s
        .project
        .items
        .iter()
        .filter(|(id, _)| s.project.sequence(**id).is_some())
        .filter_map(|(id, it)| {
            let tail = it.name.strip_prefix(&prefix)?;
            let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
            Some((id.0, it.name.clone(), digits.parse::<u32>().ok()?))
        })
        .collect();
    v.sort_by_key(|x| x.2);
    v
}

/// Keep a copy of the cut as it stands: duplicates the active sequence as `<name> · v<N> <label>`.
fn version_save(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "billo.version.save";
    let (seq, name) = sequence_name(s)?;
    let base = version_base(&name).to_string();
    let n = versions_of(s, &base).last().map_or(1, |v| v.2.saturating_add(1));
    let label: String = str_p(p, "label").unwrap_or("").chars().take(60).collect();
    let n0 = s.history.undo.len();
    let before = s.state.project_selection.clone();
    s.state.project_selection = vec![seq];
    let r = s.execute("edit.duplicate", json!({}));
    s.state.project_selection = before;
    let copy = r?.get("items").and_then(|v| v.get(0)).and_then(Value::as_u64).ok_or_else(|| bad(cmd, "the sequence could not be copied"))?;
    let title = if label.is_empty() { format!("{base} · v{n}") } else { format!("{base} · v{n} {label}") };
    let r = s.execute("clip.rename", json!({"item": copy, "name": title}));
    crate::clip_ops::collapse_history(s, n0, "Save Version");
    r?;
    Ok(json!({"version": n, "name": title, "sequence": copy}))
}

fn version_list(s: &mut Session, _p: &Value) -> Result<Value> {
    let (_, name) = sequence_name(s)?;
    let base = version_base(&name).to_string();
    let versions: Vec<Value> = versions_of(s, &base).into_iter().map(|(id, name, n)| json!({"sequence": id, "name": name, "version": n})).collect();
    Ok(json!({"base": base, "versions": versions}))
}

/// What Billo OS stores for a version of this cut (the Render Wall timeline row): the sequence,
/// its frame rate and length, and the shot lane. The caller posts it; the editor does no network.
fn version_payload(s: &mut Session, p: &Value) -> Result<Value> {
    let (_, name) = sequence_name(s)?;
    let (fps, seconds, tracks) = {
        let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let end = seq.all_tracks().flat_map(|t| t.items.iter()).map(|i| i.end()).max().unwrap_or(filmcraft_time::Tick::ZERO);
        let tracks: Vec<Value> = seq
            .video_tracks
            .iter()
            .map(|t| ("video", t))
            .chain(seq.audio_tracks.iter().map(|t| ("audio", t)))
            .map(|(kind, t)| {
                let clips: Vec<Value> = t
                    .items
                    .iter()
                    .map(|i| json!({"clip": i.id.0, "name": i.name, "start": i.start.seconds(), "duration": i.duration.seconds(), "sourceIn": i.source_in.seconds(), "enabled": i.enabled, "status": label_status(i.label)}))
                    .collect();
                json!({"kind": kind, "name": t.name, "clips": clips})
            })
            .collect();
        (seq.settings.frame_rate.as_f64(), end.seconds(), tracks)
    };
    let lane = lane(s, &json!({}))?;
    Ok(json!({
        "name": name, "label": str_p(p, "label").unwrap_or(""), "fps": fps, "durationSeconds": seconds,
        "tracks": tracks, "shots": lane["shots"], "counts": lane["counts"], "lockedPercent": lane["lockedPercent"], "editor": "Billo Cut",
    }))
}

/// Flag shots for a regen and return the ticket a generator run needs (which shot, where, how
/// long, the direction). Drafts are asked for at 480p: renders are upscaled later, never made at 1080.
fn regen_request(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "billo.regen.request";
    let prompt: String = str_p(p, "prompt").unwrap_or("").chars().take(2000).collect();
    if prompt.trim().is_empty() {
        return Err(bad(cmd, "need `prompt`: the direction for the new take"));
    }
    let clips = clip_targets(s, p);
    if clips.is_empty() {
        return Err(bad(cmd, "select the shots (or pass `clips`)"));
    }
    let note: String = prompt.chars().take(80).collect();
    set_status(s, &json!({"clips": clips.iter().map(|c| c.0).collect::<Vec<_>>(), "status": "regen", "note": note}))?;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let tickets: Vec<Value> = seq
        .video_tracks
        .iter()
        .flat_map(|t| t.items.iter())
        .filter(|i| clips.contains(&i.id))
        .map(|i| json!({"clip": i.id.0, "name": i.name, "startSeconds": i.start.seconds(), "durationSeconds": i.duration.seconds(), "prompt": prompt, "resolution": "480p", "status": "regen"}))
        .collect();
    if tickets.is_empty() {
        return Err(bad(cmd, "regens are for picture clips; none of those are on a video track"));
    }
    Ok(json!({"tickets": tickets}))
}

// ------------------------------------------------------------------------------------- Render Wall timelines

/// Most clips / tracks a Render Wall timeline may carry.
const MAX_WALL_CLIPS: usize = 5000;
const MAX_WALL_AUDIO: usize = 16;

fn wall_status(label: &str) -> Option<&'static str> {
    let l = label.to_ascii_lowercase();
    if l.contains("regen") {
        Some("regen")
    } else if l.contains("lock") {
        Some("locked")
    } else if l.contains("review") {
        Some("review")
    } else if l.contains("real") {
        Some("real")
    } else {
        None
    }
}

fn frames_of(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_f64).filter(|f| f.is_finite()).map_or(0, |f| f.clamp(0.0, 100_000_000.0) as i64)
}

/// Open a Render Wall (Billo Zord) timeline, schema `billo-timeline/1`, as a new sequence: the
/// picture track's clips at their frames with their source offsets, each audio track (record and
/// stems) on its own named track with the stems muted, and the status lane painted from the
/// timeline's status track. `items` maps the timeline's bin keys (and `track:<id>` for audio
/// tracks) to imported project items; clips whose media is not there are left as gaps and listed.
fn wall_open(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "billo.wall.open";
    let tl = p.get("timeline").filter(|t| t.is_object()).ok_or_else(|| bad(cmd, "need `timeline`: a billo-timeline/1 object"))?;
    let tracks = tl.get("tracks").and_then(Value::as_array).ok_or_else(|| bad(cmd, "the timeline has no tracks"))?;
    let item_of = |key: &str| p.get("items").and_then(|m| m.get(key)).and_then(Value::as_u64);
    let fps = tl.get("fps").and_then(Value::as_f64).filter(|f| f.is_finite()).unwrap_or(24.0).clamp(1.0, 240.0);
    let size = |i: usize, d: u64| tl.get("canvas").and_then(|c| c.get(i)).and_then(Value::as_u64).unwrap_or(d).clamp(16, 8192);
    let title: String = tl.get("title").or_else(|| tl.get("slug")).and_then(Value::as_str).unwrap_or("Render Wall Timeline").chars().take(120).collect();
    let audio: Vec<&Value> = tracks.iter().filter(|t| t["kind"] == "audio").take(MAX_WALL_AUDIO).collect();
    let picture = tracks.iter().find(|t| t["kind"] == "video" && t.get("clips").is_some_and(Value::is_array));
    let status_track =
        tracks.iter().find(|t| t["kind"] != "video" && t["kind"] != "audio" && t["name"].as_str().is_some_and(|n| n.to_ascii_lowercase().contains("status")));

    let n0 = s.history.undo.len();
    let run = |s: &mut Session| -> Result<Value> {
        let made = s.execute(
            "file.newSequence",
            json!({"name": title, "width": size(0, 1920), "height": size(1, 1080), "fps": fps, "video": 1, "audio": audio.len().max(1)}),
        )?;
        let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
        let rate = s.active_sequence().ok_or(EngineError::NoSequence)?.settings.frame_rate;
        let (mut placed, mut skipped) = (0usize, Vec::new());
        let mut blocks: Vec<(i64, i64, Label)> = Vec::new();
        for c in picture.and_then(|t| t["clips"].as_array()).map(|c| c.as_slice()).unwrap_or_default().iter().take(MAX_WALL_CLIPS) {
            let (f0, n) = (frames_of(c, "f0"), frames_of(c, "n"));
            let src = c["src"].as_str().unwrap_or_default();
            if n == 0 || src == "black" {
                continue;
            }
            let Some(item) = item_of(src) else {
                skipped.push(json!(src));
                continue;
            };
            let source_in = filmcraft_time::Tick::from_seconds_f64(c["ss"].as_f64().filter(|v| v.is_finite()).unwrap_or(0.0).clamp(0.0, 360_000.0));
            let (at, len) = (rate.tick_of(f0), rate.tick_of(f0.saturating_add(n)).0.saturating_sub(rate.tick_of(f0).0));
            s.execute(
                "timeline.place",
                json!({"item": item, "track": "V1", "time": at.0, "sourceIn": source_in.0, "duration": len, "video": true, "audio": false}),
            )?;
            let clip = s.active_sequence().and_then(|q| q.video_tracks.first()).and_then(|t| t.items.iter().find(|i| i.start == at)).map(|i| i.id);
            if let (Some(clip), Some(label)) = (clip, c["label"].as_str().filter(|l| !l.is_empty())) {
                let name: String = label.chars().take(120).collect();
                s.execute("clip.rename", json!({"clip": clip.0, "name": name}))?;
            }
            placed += 1;
        }
        // the status lane: each status block paints the picture clips that start inside it
        for g in status_track.and_then(|t| t["clips"].as_array()).map(|c| c.as_slice()).unwrap_or_default().iter().take(MAX_WALL_CLIPS) {
            if let Some(label) = g["label"].as_str().and_then(wall_status).and_then(status_label) {
                blocks.push((frames_of(g, "f0"), frames_of(g, "f0").saturating_add(frames_of(g, "n")), label));
            }
        }
        if !blocks.is_empty() {
            s.edit("Paint Shot Status", |pr, _| {
                let seq = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
                for it in seq.video_tracks.iter_mut().flat_map(|t| t.items.iter_mut()) {
                    let f = rate.frame_at(it.start);
                    if let Some(b) = blocks.iter().find(|b| f >= b.0 && f < b.1) {
                        it.label = b.2;
                    }
                }
                Ok(())
            })?;
        }
        let mut audio_tracks = Vec::new();
        for (i, t) in audio.iter().enumerate() {
            let id = t["id"].as_str().unwrap_or_default();
            let lane = format!("A{}", i + 1);
            let name: String = t["name"].as_str().unwrap_or(id).chars().take(40).collect();
            let has_media = match item_of(&format!("track:{id}")) {
                Some(item) => {
                    s.execute("timeline.place", json!({"item": item, "audioTrack": lane, "time": 0, "video": false, "audio": true}))?;
                    true
                }
                None => {
                    skipped.push(json!(format!("track:{id}")));
                    false
                }
            };
            // the record plays; its stems sit muted beside it, ready to solo
            s.execute("timeline.setTrack", json!({"track": lane, "name": name, "muted": i > 0}))?;
            audio_tracks.push(json!({"track": lane, "name": name, "media": has_media}));
        }
        Ok(json!({"sequence": made["sequence"], "name": title, "fps": fps, "placed": placed, "skipped": skipped, "audioTracks": audio_tracks}))
    };
    let r = run(s);
    crate::clip_ops::collapse_history(s, n0, "Open Render Wall Timeline");
    r
}

/// The Render Wall timeline for the cut as it stands: `base` (the timeline that was opened) with
/// its picture track rebuilt from the active sequence's V1 (gaps become black, each clip carries
/// its status), ready to save back as the next version. Clips whose media is not in the
/// timeline's bin cannot be written; they become black and are listed in `unmapped`.
fn wall_timeline(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "billo.wall.timeline";
    let mut tl = p.get("base").filter(|t| t.is_object()).cloned().ok_or_else(|| bad(cmd, "need `base`: the timeline that was opened"))?;
    let keys: Vec<(u64, String)> = p
        .get("items")
        .and_then(Value::as_object)
        .map(|m| m.iter().filter(|(k, _)| !k.starts_with("track:")).filter_map(|(k, v)| Some((v.as_u64()?, k.clone()))).collect())
        .unwrap_or_default();
    let old: Vec<Value> =
        tl["tracks"].as_array().and_then(|t| t.iter().find(|t| t["kind"] == "video")).and_then(|t| t["clips"].as_array()).cloned().unwrap_or_default();
    let mut used = vec![false; old.len()];
    let (clips, unmapped, frames) = {
        let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let rate = seq.settings.frame_rate;
        let mut items: Vec<&filmcraft_project::TrackItem> = seq.video_tracks.first().map(|t| t.items.iter().collect()).unwrap_or_default();
        items.sort_by_key(|i| i.start);
        let (mut out, mut unmapped, mut cursor, mut gaps) = (Vec::new(), Vec::new(), 0i64, 0usize);
        let black = |f0: i64, n: i64, k: usize| json!({"id": format!("g{k}"), "src": "black", "ss": 0.0, "n": n, "f0": f0, "label": "Black", "lut": null});
        for it in items.into_iter().take(MAX_WALL_CLIPS) {
            let f0 = rate.frame_at(it.start).max(cursor);
            let n = rate.frame_at(it.end()).saturating_sub(f0);
            if n <= 0 {
                continue;
            }
            if f0 > cursor {
                gaps += 1;
                out.push(black(cursor, f0 - cursor, gaps));
            }
            match keys.iter().find(|(id, _)| *id == it.item.0) {
                Some((_, src)) => {
                    let prior = old
                        .iter()
                        .enumerate()
                        .find(|(i, c)| !used.get(*i).copied().unwrap_or(true) && c["src"] == src.as_str() && c["label"] == it.name.as_str());
                    let mut clip = match prior {
                        Some((i, c)) => {
                            if let Some(u) = used.get_mut(i) {
                                *u = true;
                            }
                            c.clone()
                        }
                        None => json!({"id": format!("c{}", it.id.0), "lut": null}),
                    };
                    clip["src"] = json!(src);
                    clip["ss"] = json!((it.source_in.seconds() * 1000.0).round() / 1000.0);
                    clip["n"] = json!(n);
                    clip["f0"] = json!(f0);
                    clip["label"] = json!(it.name);
                    clip["status"] = json!(label_status(it.label));
                    out.push(clip);
                }
                None => {
                    gaps += 1;
                    unmapped.push(json!(it.name));
                    out.push(black(f0, n, gaps));
                }
            }
            cursor = f0.saturating_add(n);
        }
        (out, unmapped, cursor)
    };
    let fps = tl["fps"].as_f64().filter(|f| f.is_finite() && *f > 0.0).unwrap_or(24.0);
    let video = tl["tracks"].as_array_mut().and_then(|t| t.iter_mut().find(|t| t["kind"] == "video")).ok_or_else(|| bad(cmd, "`base` has no picture track"))?;
    video["clips"] = Value::Array(clips);
    tl["frames"] = json!(frames);
    tl["duration"] = json!(frames as f64 / fps);
    tl["editor"] = json!("Billo Cut");
    Ok(json!({"timeline": tl, "unmapped": unmapped, "frames": frames}))
}

// ------------------------------------------------------------------------------------- upscale

/// Queue clips (or the whole cut) for the upscale runner: returns the job rows Billo OS stores
/// (`os.upscale_jobs`) and marks each shot. The editor does not run the upscale; the runner does,
/// in two 2x passes with a face check between them, and hands back a proxy.
fn upscale_request(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "billo.upscale.request";
    let target = match str_p(p, "target").unwrap_or("4k").to_ascii_lowercase().as_str() {
        "960p" => "960p",
        "1920p" | "1080p" => "1920p",
        "4k" | "2160p" => "4k",
        _ => return Err(bad(cmd, "`target` must be 960p, 1920p or 4k")),
    };
    let tag = match str_p(p, "tag").unwrap_or("ai_gen").to_ascii_lowercase().as_str() {
        "ai_gen" => "ai_gen",
        "real" | "real_footage" | "stage" => "real_footage",
        "bulk" => "bulk",
        "anime" => "anime",
        _ => return Err(bad(cmd, "`tag` must be ai_gen, real, bulk or anime")),
    };
    let engine: String = str_p(p, "engine").unwrap_or("auto").chars().take(40).collect();
    let whole = p.get("timeline").and_then(Value::as_bool).unwrap_or(false);
    let clips = clip_targets(s, p);
    let (project, source, jobs) = {
        let (_, project) = sequence_name(s)?;
        let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let source = format!("{}x{}", seq.settings.width, seq.settings.height);
        let jobs: Vec<(i64, String, Value)> = seq
            .video_tracks
            .iter()
            .flat_map(|t| t.items.iter())
            .filter(|i| whole || clips.contains(&i.id))
            .take(MAX_WALL_CLIPS)
            .map(|i| {
                // shaped as an os.upscale_jobs row: `engine_id` stays empty for the runner's own routing by tag
                let job = json!({
                    "take_ref": i.name, "project": version_base(&project), "source_res": source, "target_res": target,
                    "engine_id": if engine == "auto" { Value::Null } else { json!(engine) },
                    "passes": [{"scale": 2}, {"qc": "face-lock"}, {"scale": 2}], "status": "queued",
                    "ctx": {"clip": i.id.0, "tag": tag, "engine": engine, "startSeconds": i.start.seconds(), "durationSeconds": i.duration.seconds(), "editor": "Billo Cut"},
                });
                (i.start.0, i.name.clone(), job)
            })
            .collect();
        (project, source, jobs)
    };
    if jobs.is_empty() {
        return Err(bad(cmd, "select the shots to upscale (or pass `timeline: true` for the whole cut)"));
    }
    let n0 = s.history.undo.len();
    let mut marked: Vec<i64> = Vec::new();
    let mut failed = None;
    for (at, name, _) in &jobs {
        if marked.contains(at) {
            continue;
        }
        marked.push(*at);
        if let Err(e) = s.execute("markers.add", json!({"time": at, "name": format!("UPSCALE to {}: {name}", target.to_ascii_uppercase())})) {
            failed = Some(e);
            break;
        }
    }
    crate::clip_ops::collapse_history(s, n0, "Request Upscale");
    if let Some(e) = failed {
        return Err(e);
    }
    Ok(json!({"project": version_base(&project), "source_res": source, "target_res": target, "jobs": jobs.into_iter().map(|j| j.2).collect::<Vec<_>>()}))
}

// ------------------------------------------------------------------------------------- beats, dead air

/// Most beat markers / silence cuts one command makes.
const MAX_BEATS: usize = 2000;
const BEAT_PREFIX: &str = "BEAT";

/// A clip's own signal folded to mono, with the sequence sample rate and the clip's start.
fn clip_mono(s: &Session, clip: ClipId, cmd: &str, max: usize) -> Result<(Vec<f32>, u32, filmcraft_time::Tick)> {
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let sr = seq.settings.sample_rate.max(1);
    let it = seq
        .audio_tracks
        .iter()
        .flat_map(|t| t.items.iter())
        .find(|i| i.id == clip)
        .ok_or_else(|| bad(cmd, "that is not an audio clip in the active sequence"))?;
    let (a0, a1) = (it.start.to_units_floor(sr as i64), it.end().to_units_floor(sr as i64));
    let total = usize::try_from(a1.saturating_sub(a0)).unwrap_or(0);
    if total == 0 {
        return Err(bad(cmd, "the clip is empty"));
    }
    if total > max {
        return Err(bad(cmd, "the clip is too long; cut it down first"));
    }
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let mut mono = Vec::new();
    mono.try_reserve(total).map_err(|e| bad(cmd, format!("not enough memory: {e}")))?;
    let mut pos = a0;
    while pos < a1 {
        let n = usize::try_from((a1 - pos).min(sr as i64)).unwrap_or(0).max(1);
        let [l, r] = filmcraft_render::audio::clip_signal(it, pos, n, sr, &provider).ok_or_else(|| bad(cmd, "the clip has no audio"))?;
        mono.extend(l.iter().zip(&r).map(|(a, b)| 0.5 * (a + b)));
        pos = pos.saturating_add(n as i64);
    }
    Ok((mono, sr, it.start))
}

/// Find the hits in a song clip (kicks by default) and drop a `BEAT` marker on each.
fn beats_detect(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "billo.beats.detect";
    let song = song_target(s, p, cmd)?;
    let band = match str_p(p, "band").unwrap_or("kick").to_ascii_lowercase().as_str() {
        "kick" => filmcraft_audio_dsp::beats::Band::Kick,
        "all" => filmcraft_audio_dsp::beats::Band::All,
        _ => return Err(bad(cmd, "`band` must be kick or all")),
    };
    let (mono, sr, start) = clip_mono(s, song, cmd, filmcraft_audio_dsp::beats::MAX_SAMPLES)?;
    let hits = filmcraft_audio_dsp::beats::onsets(&mono, sr, band);
    let bpm = filmcraft_audio_dsp::beats::tempo(&hits, sr);
    drop(mono);
    let rate = s.active_sequence().ok_or(EngineError::NoSequence)?.settings.frame_rate;
    let mut times: Vec<filmcraft_time::Tick> = hits
        .iter()
        .map(|h| rate.snap_nearest(filmcraft_time::Tick(start.0.saturating_add(filmcraft_time::Tick::from_units(*h as i64, sr as i64).0))))
        .collect();
    times.dedup();
    let truncated = times.len() > MAX_BEATS;
    times.truncate(MAX_BEATS);
    if p.get("markers").and_then(Value::as_bool).unwrap_or(true) && !times.is_empty() {
        let n0 = s.history.undo.len();
        // replace the last pass's beat markers rather than stacking a second set
        let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
        let mut failed = s
            .edit("Clear Beat Markers", |pr, _| {
                let seq = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
                seq.markers.retain(|m| !m.name.starts_with(BEAT_PREFIX));
                Ok(())
            })
            .err();
        for (i, t) in times.iter().enumerate() {
            if failed.is_some() {
                break;
            }
            failed = s.execute("markers.add", json!({"time": t.0, "name": format!("{BEAT_PREFIX} {}", i + 1)})).err();
        }
        crate::clip_ops::collapse_history(s, n0, "Mark Beats");
        if let Some(e) = failed {
            return Err(e);
        }
    }
    Ok(json!({"song": song.0, "count": times.len(), "bpm": bpm, "truncated": truncated, "beats": times.iter().map(|t| t.seconds()).collect::<Vec<_>>()}))
}

/// Roll every cut on the picture track onto the nearest `BEAT` marker within `tolerance` frames.
fn beats_snap_cuts(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "billo.beats.snapCuts";
    let tolerance = p.get("tolerance").and_then(Value::as_i64).unwrap_or(6).clamp(1, 48);
    let (cuts, beats, rate) = {
        let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let rate = seq.settings.frame_rate;
        let beats: Vec<i64> = seq.markers.iter().filter(|m| m.name.starts_with(BEAT_PREFIX)).map(|m| rate.frame_at(m.start)).collect();
        let track = seq.video_tracks.first().ok_or_else(|| bad(cmd, "no picture track"))?;
        let mut items: Vec<&filmcraft_project::TrackItem> = track.items.iter().collect();
        items.sort_by_key(|i| i.start);
        let cuts: Vec<(ClipId, ClipId, i64)> = items
            .windows(2)
            .filter_map(|w| Some((w.first()?, w.get(1)?)))
            .filter(|(a, b)| a.end() == b.start)
            .map(|(a, b)| (a.id, b.id, rate.frame_at(b.start)))
            .take(MAX_WALL_CLIPS)
            .collect();
        (cuts, beats, rate)
    };
    if beats.is_empty() {
        return Err(bad(cmd, "no beat markers yet: run billo.beats.detect on the song first"));
    }
    let n0 = s.history.undo.len();
    let (mut moved, mut already, mut out_of_reach, mut blocked) = (0usize, 0usize, 0usize, 0usize);
    for (left, right, at) in &cuts {
        let Some(delta) = beats.iter().map(|b| b - at).min_by_key(|d| d.abs()) else { continue };
        if delta == 0 {
            already += 1;
        } else if delta.abs() > tolerance {
            out_of_reach += 1;
        } else if s.execute("timeline.roll", json!({"left": left.0, "right": right.0, "delta": rate.tick_of(delta).0})).is_ok() {
            moved += 1;
        } else {
            blocked += 1;
        }
    }
    crate::clip_ops::collapse_history(s, n0, "Snap Cuts to Beats");
    let on_beat = moved + already;
    Ok(json!({"cuts": cuts.len(), "moved": moved, "alreadyOnBeat": already, "outOfReach": out_of_reach, "blocked": blocked,
        "onBeatPercent": on_beat.saturating_mul(100).checked_div(cuts.len()).unwrap_or(0)}))
}

/// Cut the dead air out of a talking clip: every stretch quieter than `thresholdDb` for at least
/// `minSilence` seconds is removed from all unlocked tracks (keeping `keep` seconds of breath on
/// each side) and the cut closes up. One undo step. `dryRun` only reports what it would remove.
fn cut_silence(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "billo.cut.silence";
    let clip = song_target(s, &json!({"song": p.get("clip").cloned().unwrap_or(Value::Null)}), cmd)?;
    let threshold = p.get("thresholdDb").and_then(Value::as_f64).unwrap_or(-42.0).clamp(-90.0, -6.0) as f32;
    let min_silence = p.get("minSilence").and_then(Value::as_f64).unwrap_or(0.7).clamp(0.2, 30.0) as f32;
    let keep = p.get("keep").and_then(Value::as_f64).unwrap_or(0.12).clamp(0.0, 2.0);
    let (mono, sr, start) = clip_mono(s, clip, cmd, filmcraft_audio_dsp::beats::MAX_SAMPLES)?;
    let runs = filmcraft_audio_dsp::beats::quiet_runs(&mono, sr, threshold, min_silence);
    drop(mono);
    let rate = s.active_sequence().ok_or(EngineError::NoSequence)?.settings.frame_rate;
    let pad = filmcraft_time::Tick::from_seconds_f64(keep).0;
    let tick = |n: usize| start.0.saturating_add(filmcraft_time::Tick::from_units(n as i64, sr as i64).0);
    let ranges: Vec<filmcraft_time::TimeRange> = runs
        .iter()
        .map(|(a, b)| {
            (rate.snap_nearest(filmcraft_time::Tick(tick(*a).saturating_add(pad))), rate.snap_nearest(filmcraft_time::Tick(tick(*b).saturating_sub(pad))))
        })
        .filter(|(a, b)| b.0 > a.0)
        .map(|(a, b)| filmcraft_time::TimeRange::from_bounds(a, b))
        .take(MAX_BEATS)
        .collect();
    let seconds: f64 = ranges.iter().map(|r| r.duration.seconds()).sum();
    let report: Vec<Value> = ranges.iter().map(|r| json!({"start": r.start.seconds(), "end": r.end().seconds()})).collect();
    if ranges.is_empty() || p.get("dryRun").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(json!({"clip": clip.0, "removed": 0, "found": ranges.len(), "seconds": seconds, "ranges": report}));
    }
    s.edit_sequence("Cut Dead Air", |q, ctx, st| {
        let tracks: Vec<filmcraft_project::TrackId> = q.all_tracks().map(|t| t.id).collect();
        // last first, so earlier ranges keep their place
        for r in ranges.iter().rev() {
            filmcraft_edit::extract(q, &tracks, *r, ctx);
            if st.ripple_sequence_markers {
                crate::sequence_tools::ripple_markers(&mut q.markers, r.end(), -r.duration);
            }
        }
        q.check().map_err(EngineError::Other)?;
        Ok(())
    })?;
    Ok(json!({"clip": clip.0, "removed": ranges.len(), "found": ranges.len(), "seconds": seconds, "ranges": report}))
}

// ------------------------------------------------------------------------------------- assistant

/// One thing the assistant understands: words that must all appear, the command, and its params.
struct Intent {
    words: &'static [&'static str],
    id: &'static str,
    params: &'static str,
    says: &'static str,
}

/// Plain-language requests the assistant routes, first match wins (longer phrases first). No
/// model: a fixed list, so what it can do is exactly what is written here.
const INTENTS: &[Intent] = &[
    Intent { words: &["split", "stem"], id: "billo.stems.split", params: "{}", says: "Split the selected song into Vocals, Drums, Bass and Other" },
    Intent { words: &["separate", "stem"], id: "billo.stems.split", params: "{}", says: "Split the selected song into stems" },
    Intent { words: &["solo", "vocal"], id: "billo.stems.solo", params: r#"{"name":"Vocals"}"#, says: "Solo the Vocals stem" },
    Intent { words: &["solo", "drum"], id: "billo.stems.solo", params: r#"{"name":"Drums"}"#, says: "Solo the Drums stem" },
    Intent { words: &["solo", "bass"], id: "billo.stems.solo", params: r#"{"name":"Bass"}"#, says: "Solo the Bass stem" },
    Intent { words: &["solo", "other"], id: "billo.stems.solo", params: r#"{"name":"Other"}"#, says: "Solo the Other stem" },
    Intent { words: &["clear", "solo"], id: "billo.stems.solo", params: "{}", says: "Clear the stem solo" },
    Intent { words: &["unsolo"], id: "billo.stems.solo", params: "{}", says: "Clear the stem solo" },
    Intent { words: &["needs", "regen"], id: "billo.shot.setStatus", params: r#"{"status":"regen"}"#, says: "Mark the selected shots as needing a regen" },
    Intent { words: &["mark", "regen"], id: "billo.shot.setStatus", params: r#"{"status":"regen"}"#, says: "Mark the selected shots as needing a regen" },
    Intent { words: &["mark", "lock"], id: "billo.shot.setStatus", params: r#"{"status":"locked"}"#, says: "Mark the selected shots locked" },
    Intent { words: &["lock", "shot"], id: "billo.shot.setStatus", params: r#"{"status":"locked"}"#, says: "Mark the selected shots locked" },
    Intent { words: &["mark", "real"], id: "billo.shot.setStatus", params: r#"{"status":"real"}"#, says: "Mark the selected shots as real footage" },
    Intent { words: &["mark", "review"], id: "billo.shot.setStatus", params: r#"{"status":"review"}"#, says: "Mark the selected shots for review" },
    Intent { words: &["what", "lock"], id: "billo.shot.lane", params: "{}", says: "Show the shot lane" },
    Intent { words: &["shot", "lane"], id: "billo.shot.lane", params: "{}", says: "Show the shot lane" },
    Intent { words: &["status"], id: "billo.shot.lane", params: "{}", says: "Show the shot lane" },
    Intent { words: &["clean", "dialog"], id: "billo.dialogue.clean", params: "{}", says: "Clean up the selected dialogue" },
    Intent { words: &["clean", "voice"], id: "billo.dialogue.clean", params: "{}", says: "Clean up the selected dialogue" },
    Intent { words: &["loudness"], id: "billo.deliver.loudness", params: "{}", says: "Bring the timeline to the delivery loudness" },
    Intent { words: &["lufs"], id: "billo.deliver.loudness", params: "{}", says: "Bring the timeline to the delivery loudness" },
    Intent { words: &["save", "version"], id: "billo.version.save", params: "{}", says: "Keep a copy of the cut as a new version" },
    Intent { words: &["new", "version"], id: "billo.version.save", params: "{}", says: "Keep a copy of the cut as a new version" },
    Intent { words: &["list", "version"], id: "billo.version.list", params: "{}", says: "List the versions of this cut" },
    Intent { words: &["swap", "take"], id: "billo.shot.swapTake", params: "{}", says: "Swap the take picked in the Project panel into the selected shot" },
    Intent { words: &["vault", "lut"], id: "billo.fxVault.list", params: r#"{"category":"luts"}"#, says: "List the LUTs in the FX Vault" },
    Intent { words: &["vault", "transition"], id: "billo.fxVault.list", params: r#"{"category":"transitions"}"#, says: "List the transitions in the FX Vault" },
    Intent { words: &["vault", "sound"], id: "billo.fxVault.list", params: r#"{"category":"sfx"}"#, says: "List the sound effects in the FX Vault" },
    Intent { words: &["vault"], id: "billo.fxVault.list", params: "{}", says: "List the FX Vault" },
    Intent { words: &["snap", "beat"], id: "billo.beats.snapCuts", params: "{}", says: "Roll the cuts onto the nearest beat" },
    Intent { words: &["cut", "on", "kick"], id: "billo.beats.snapCuts", params: "{}", says: "Roll the cuts onto the nearest kick" },
    Intent { words: &["cut", "on", "beat"], id: "billo.beats.snapCuts", params: "{}", says: "Roll the cuts onto the nearest beat" },
    Intent { words: &["beat"], id: "billo.beats.detect", params: "{}", says: "Find the beats in the selected song and mark them" },
    Intent { words: &["kick"], id: "billo.beats.detect", params: "{}", says: "Find the kicks in the selected song and mark them" },
    Intent { words: &["dead", "air"], id: "billo.cut.silence", params: "{}", says: "Cut the dead air out of the selected clip" },
    Intent { words: &["silence"], id: "billo.cut.silence", params: "{}", says: "Cut the silences out of the selected clip" },
    Intent { words: &["master", "4k"], id: "billo.upscale.request", params: r#"{"timeline":true,"target":"4k"}"#, says: "Queue the whole cut for a 4K master" },
    Intent { words: &["upscale"], id: "billo.upscale.request", params: "{}", says: "Queue the selected shots for a 4K upscale" },
    Intent { words: &["duck"], id: "essentialSound.generateDucking", params: "{}", says: "Duck the music under the dialogue" },
    Intent { words: &["remove", "pause"], id: "transcript.removePauses", params: "{}", says: "Cut the pauses out of the transcript" },
    Intent { words: &["remove", "filler"], id: "transcript.removeFillers", params: "{}", says: "Cut the filler words out of the transcript" },
    Intent { words: &["caption"], id: "transcript.createCaptions", params: "{}", says: "Make captions from the transcript" },
    Intent { words: &["transcri"], id: "transcript.generate", params: "{}", says: "Transcribe the sequence" },
    Intent { words: &["scene", "detect"], id: "clip.sceneEditDetection", params: "{}", says: "Find the cuts in the selected clip" },
    Intent { words: &["detect", "cut"], id: "clip.sceneEditDetection", params: "{}", says: "Find the cuts in the selected clip" },
    Intent { words: &["ripple", "delete"], id: "edit.rippleDelete", params: "{}", says: "Ripple delete the selection" },
    Intent { words: &["cut", "here"], id: "sequence.addEdit", params: "{}", says: "Cut at the playhead" },
    Intent { words: &["razor"], id: "sequence.addEdit", params: "{}", says: "Cut at the playhead" },
    Intent { words: &["add", "marker"], id: "markers.add", params: "{}", says: "Add a marker at the playhead" },
    Intent { words: &["nest"], id: "clip.nest", params: "{}", says: "Nest the selection" },
    Intent { words: &["redo"], id: "edit.redo", params: "{}", says: "Redo" },
    Intent { words: &["undo"], id: "edit.undo", params: "{}", says: "Undo" },
];

/// Numbers the request carries that a command takes (`-16 lufs`, a version label, a marker name).
fn assistant_params(id: &str, text: &str, base: &str) -> Value {
    let mut params: Value = serde_json::from_str(base).unwrap_or_else(|_| json!({}));
    let lower = text.to_ascii_lowercase();
    if id == "billo.deliver.loudness" || id == "billo.dialogue.clean" {
        let number = lower
            .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '−'))
            .filter_map(|w| w.replace('−', "-").parse::<f64>().ok())
            .find(|v| v.is_finite() && v.abs() >= 5.0 && v.abs() <= 40.0);
        if let Some(v) = number {
            params["target"] = json!(-v.abs());
        }
    }
    let after = |word: &str| {
        lower
            .find(word)
            .and_then(|at| text.get(at + word.len()..))
            .map(|rest| rest.trim_matches(|c: char| c.is_whitespace() || c == ':' || c == '"').chars().take(60).collect::<String>())
            .filter(|v| !v.is_empty())
    };
    if id == "billo.version.save"
        && let Some(label) = after("version")
    {
        params["label"] = json!(label);
    }
    if id == "markers.add"
        && let Some(name) = after("marker")
    {
        params["name"] = json!(name);
    }
    params
}

fn assistant_match(text: &str) -> Option<&'static Intent> {
    let lower = text.to_ascii_lowercase();
    INTENTS.iter().find(|i| i.words.iter().all(|w| lower.contains(w)))
}

/// Run a plain-language request. Answers `{understood:false, try:[…]}` when nothing matches;
/// `dryRun` reports what it would do without doing it.
fn assistant_run(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "billo.assistant.run";
    let text: String = str_p(p, "text").unwrap_or("").chars().take(400).collect();
    if text.trim().is_empty() {
        return Err(bad(cmd, "need `text`: what to do"));
    }
    let Some(intent) = assistant_match(&text) else {
        let examples: Vec<&str> =
            ["split the stems", "solo vocals", "mark locked", "clean dialogue", "deliver at -14 lufs", "save version rough cut", "show the vault luts"]
                .to_vec();
        return Ok(json!({"understood": false, "try": examples}));
    };
    let params = assistant_params(intent.id, &text, intent.params);
    if p.get("dryRun").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(json!({"understood": true, "command": intent.id, "params": params, "says": intent.says, "ran": false}));
    }
    match s.execute(intent.id, params.clone()) {
        Ok(result) => Ok(json!({"understood": true, "command": intent.id, "params": params, "says": intent.says, "ran": true, "result": result})),
        Err(e) => Ok(json!({"understood": true, "command": intent.id, "params": params, "says": intent.says, "ran": false, "error": e.to_string()})),
    }
}

fn assistant_catalog(_s: &mut Session, _p: &Value) -> Result<Value> {
    let mut seen: Vec<&str> = Vec::new();
    let list: Vec<Value> = INTENTS
        .iter()
        .filter(|i| {
            let fresh = !seen.contains(&i.says);
            seen.push(i.says);
            fresh
        })
        .map(|i| json!({"say": i.words.join(" "), "does": i.says, "command": i.id}))
        .collect();
    Ok(json!({"count": list.len(), "requests": list}))
}

fn spec(id: &'static str, label: &'static str, params: &'static str, enabled: Enabled, run: Run, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled, run, journal }
}

fn menu(id: &'static str, label: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &["Billo"], shortcut: None, params: "{}", enabled: has_seq, run, journal: true }
}

fn mark(s: &mut Session, status: &str) -> Result<Value> {
    set_status(s, &json!({"status": status}))
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("billo.shot.setStatus", "Set Shot Status", r#"{"clips":[id]?,"status":"locked|real|regen|review","note":str?}"#, has_seq, set_status, true),
        spec("billo.shot.lane", "Shot Status Lane", "{}", has_seq, lane, false),
        spec(
            "billo.stems.lay",
            "Lay Stems Under Song",
            r#"{"song":clipId,"stems":[{"item":id,"name":str}],"muteSong":bool=false,"fromClipStart":bool=false}"#,
            has_seq,
            stems_lay,
            true,
        ),
        spec("billo.stems.solo", "Solo Stem", r#"{"name":str?}"#, has_seq, stems_solo, true),
        spec("billo.stems.split", "Split Stems", r#"{"song":clipId?,"muteSong":bool=true,"dir":str?}"#, has_seq, stems_split, true),
        spec("billo.shot.swapTake", "Swap Take", r#"{"clip":id?,"item":id?}"#, has_seq, swap_take, true),
        spec("billo.version.save", "Save Version", r#"{"label":str?}"#, has_seq, version_save, true),
        spec("billo.version.list", "Versions", "{}", has_seq, version_list, false),
        spec("billo.version.payload", "Version Payload", r#"{"label":str?}"#, has_seq, version_payload, false),
        spec("billo.regen.request", "Request Regen", r#"{"clips":[id]?,"prompt":str}"#, has_seq, regen_request, true),
        spec("billo.wall.open", "Open Render Wall Timeline", r#"{"timeline":billo-timeline/1,"items":{binKey|"track:<id>":itemId}}"#, always, wall_open, true),
        spec("billo.wall.timeline", "Render Wall Timeline", r#"{"base":billo-timeline/1,"items":{binKey:itemId}}"#, has_seq, wall_timeline, false),
        spec(
            "billo.upscale.request",
            "Request Upscale",
            r#"{"clips":[id]?,"timeline":bool=false,"target":"960p|1920p|4k"="4k","tag":"ai_gen|real|bulk|anime"="ai_gen","engine":str="auto"}"#,
            has_seq,
            upscale_request,
            true,
        ),
        spec("billo.beats.detect", "Find the Beats", r#"{"song":clipId?,"band":"kick|all"="kick","markers":bool=true}"#, has_seq, beats_detect, true),
        spec("billo.beats.snapCuts", "Snap Cuts to the Beat", r#"{"tolerance":frames=6}"#, has_seq, beats_snap_cuts, true),
        spec(
            "billo.cut.silence",
            "Cut Dead Air",
            r#"{"clip":clipId?,"thresholdDb":f64=-42,"minSilence":seconds=0.7,"keep":seconds=0.12,"dryRun":bool=false}"#,
            has_seq,
            cut_silence,
            true,
        ),
        spec("billo.assistant.run", "Ask Billo", r#"{"text":str,"dryRun":bool=false}"#, always, assistant_run, true),
        spec("billo.assistant.catalog", "What Billo Understands", "{}", always, assistant_catalog, false),
        spec("billo.dialogue.clean", "Clean Dialogue", r#"{"clips":[id]?,"target":lufs=-16}"#, has_seq, dialogue_clean, true),
        spec("billo.deliver.loudness", "Deliver Loudness", r#"{"target":lufs=-14}"#, has_seq, deliver_loudness, true),
        spec("billo.wall.package", "Render Wall Package", r#"{"dir":str}"#, has_seq, wall_package, true),
        menu("billo.shot.markLocked", "Mark Shot Locked", |s, _| mark(s, "locked")),
        menu("billo.shot.markReal", "Mark Shot Real Footage", |s, _| mark(s, "real")),
        menu("billo.shot.markRegen", "Mark Shot Needs Regen", |s, _| mark(s, "regen")),
        menu("billo.shot.markReview", "Mark Shot For Review", |s, _| mark(s, "review")),
        menu("billo.dialogue.cleanSelected", "Clean Up Dialogue", |s, _| dialogue_clean(s, &json!({}))),
        menu("billo.deliver.loudness14", "Deliver at −14 LUFS", |s, _| deliver_loudness(s, &json!({}))),
        menu("billo.stems.clearSolo", "Clear Stem Solo", |s, _| stems_solo(s, &json!({}))),
        menu("billo.stems.splitSelected", "Split Song Into Stems", |s, _| stems_split(s, &json!({}))),
        menu("billo.version.saveNow", "Save Version", |s, _| version_save(s, &json!({}))),
        menu("billo.beats.detectSelected", "Find the Beats in Song", |s, _| beats_detect(s, &json!({}))),
        menu("billo.beats.snapAll", "Snap Cuts to the Beat", |s, _| beats_snap_cuts(s, &json!({}))),
        menu("billo.cut.silenceSelected", "Cut Dead Air", |s, _| cut_silence(s, &json!({}))),
        menu("billo.upscale.selected4k", "Upscale Shot to 4K", |s, _| upscale_request(s, &json!({}))),
        menu("billo.upscale.master4k", "Master in 4K", |s, _| upscale_request(s, &json!({"timeline": true}))),
        menu("billo.shot.swapTakeSelected", "Swap In Selected Take", |s, _| swap_take(s, &json!({}))),
        spec(
            "billo.fxVault.list",
            "FX Vault",
            r#"{"category":"transitions|luts|text|vocal|sfx|plugins"?,"status":"vault|resend|store|licensed"?}"#,
            always,
            fx_vault_list,
            false,
        ),
    ]
}
