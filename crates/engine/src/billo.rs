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
        (it.start, it.source_in, it.duration)
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
    for f in folders {
        std::fs::create_dir_all(format!("{dir}/{f}")).map_err(|e| bad(cmd, format!("{dir}/{f}: {e}")))?;
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
    std::fs::write(&idx, body).map_err(|e| bad(cmd, format!("{idx}: {e}")))?;
    written.push(idx);
    let readme = format!(
        "# {name}\n\nBillo Cut Render Wall package.\n\n- 01_MEDIA source media\n- 02_AUDIO stems\n- 03_TRANSCRIPT transcript and captions\n- 04_INDEX shots.json (status lane: {} shots, {}% locked)\n- 05_TIMELINE OTIO / FCPXML / EDL\n- 06_RENDERS renders\n- 07_QC teardown\n",
        lane["total"], lane["lockedPercent"]
    );
    let rp = format!("{dir}/README.md");
    std::fs::write(&rp, readme).map_err(|e| bad(cmd, format!("{rp}: {e}")))?;
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

fn spec(id: &'static str, label: &'static str, params: &'static str, enabled: Enabled, run: Run, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled, run, journal }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("billo.shot.setStatus", "Set Shot Status", r#"{"clips":[id]?,"status":"locked|real|regen|review","note":str?}"#, has_seq, set_status, true),
        spec("billo.shot.lane", "Shot Status Lane", "{}", has_seq, lane, false),
        spec("billo.stems.lay", "Lay Stems Under Song", r#"{"song":clipId,"stems":[{"item":id,"name":str}],"muteSong":bool=false}"#, has_seq, stems_lay, true),
        spec("billo.stems.solo", "Solo Stem", r#"{"name":str?}"#, has_seq, stems_solo, true),
        spec("billo.dialogue.clean", "Clean Dialogue", r#"{"clips":[id]?,"target":lufs=-16}"#, has_seq, dialogue_clean, true),
        spec("billo.deliver.loudness", "Deliver Loudness", r#"{"target":lufs=-14}"#, has_seq, deliver_loudness, true),
        spec("billo.wall.package", "Render Wall Package", r#"{"dir":str}"#, has_seq, wall_package, true),
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
