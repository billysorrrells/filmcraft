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

fn label_status(l: Label) -> Option<&'static str> {
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

fn fx_vault_list(_s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "billo.fxVault.list";
    let all: Vec<Value> = serde_json::from_str(FX_VAULT).map_err(|e| bad(cmd, e.to_string()))?;
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
fn version_base(name: &str) -> &str {
    name.rsplit_once(" · v").filter(|(_, tail)| tail.chars().next().is_some_and(|c| c.is_ascii_digit())).map_or(name, |(base, _)| base)
}

fn versions_of(s: &Session, base: &str) -> Vec<(u64, String, u32)> {
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
