//! Billo Cut commands: shot status lane, stem lanes, FX Vault, Render Wall package.

use serde_json::json;

use super::*;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn first_clip(s: &Session, audio: bool) -> (u64, u64) {
    let seq = s.active_sequence().unwrap();
    let tracks = if audio { &seq.audio_tracks } else { &seq.video_tracks };
    let it = tracks.iter().flat_map(|t| t.items.iter()).next().unwrap();
    (it.id.0, it.item.0)
}

#[test]
fn fx_vault_lists_and_filters() {
    let mut s = Session::default();
    let all = s.execute("billo.fxVault.list", json!({})).unwrap();
    assert_eq!(all["count"], 43);
    let luts = s.execute("billo.fxVault.list", json!({"category": "luts"})).unwrap();
    assert_eq!(luts["count"], 3);
    let resend = s.execute("billo.fxVault.list", json!({"status": "resend"})).unwrap();
    assert_eq!(resend["count"], 17);
}

#[test]
fn shot_status_paints_label_marks_and_counts() {
    let mut s = demo();
    let (clip, _) = first_clip(&s, false);
    let before = s.active_sequence().unwrap().markers.len();
    s.execute("billo.shot.setStatus", json!({"clips": [clip], "status": "locked", "note": "hero shot"})).unwrap();
    let seq = s.active_sequence().unwrap();
    let it = seq.video_tracks.iter().flat_map(|t| t.items.iter()).find(|i| i.id.0 == clip).unwrap();
    assert_eq!(it.label, filmcraft_project::Label::Forest);
    assert_eq!(seq.markers.len(), before + 1);
    let lane = s.execute("billo.shot.lane", json!({})).unwrap();
    assert_eq!(lane["counts"]["locked"], 1);
    assert!(lane["total"].as_u64().unwrap() >= 1);
    s.execute("edit.undo", json!({})).unwrap();
}

#[test]
fn shot_status_rejects_unknown_and_empty() {
    let mut s = demo();
    let (clip, _) = first_clip(&s, false);
    assert!(s.execute("billo.shot.setStatus", json!({"clips": [clip], "status": "maybe"})).is_err());
    assert!(s.execute("billo.shot.setStatus", json!({"clips": [], "status": "locked"})).is_err());
    assert!(s.execute("billo.shot.setStatus", json!({"clips": [999_999_999u64], "status": "regen"})).is_err());
}

#[test]
fn stems_lay_on_named_tracks_in_sync_and_solo() {
    let mut s = demo();
    let (song, item) = first_clip(&s, true);
    let n0 = s.active_sequence().unwrap().audio_tracks.len();
    let r = s
        .execute("billo.stems.lay", json!({"song": song, "stems": [{"item": item, "name": "Vocals"}, {"item": item, "name": "Drums"}], "muteSong": true}))
        .unwrap();
    assert_eq!(r["stems"].as_array().unwrap().len(), 2);
    let seq = s.active_sequence().unwrap();
    assert_eq!(seq.audio_tracks.len(), n0 + 2);
    let names: Vec<&str> = seq.audio_tracks.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"Vocals") && names.contains(&"Drums"), "{names:?}");
    let song_start = seq.audio_tracks.iter().flat_map(|t| t.items.iter()).find(|i| i.id.0 == song).unwrap().start;
    let voc = seq.audio_tracks.iter().find(|t| t.name == "Vocals").unwrap();
    assert_eq!(voc.items.first().unwrap().start, song_start);
    s.execute("billo.stems.solo", json!({"name": "vocals"})).unwrap();
    assert!(s.execute("billo.stems.solo", json!({"name": "nope"})).is_err());
    s.execute("billo.stems.solo", json!({})).unwrap();
}

#[test]
fn stems_lay_rejects_bad_input() {
    let mut s = demo();
    let (song, item) = first_clip(&s, true);
    assert!(s.execute("billo.stems.lay", json!({"song": song, "stems": []})).is_err());
    let many: Vec<_> = (0..9).map(|_| json!({"item": item, "name": "x"})).collect();
    assert!(s.execute("billo.stems.lay", json!({"song": song, "stems": many})).is_err());
    assert!(s.execute("billo.stems.lay", json!({"song": 123_456_789u64, "stems": [{"item": item}]})).is_err());
}

#[test]
fn wall_package_writes_folders_and_index() {
    let mut s = demo();
    let dir = std::env::temp_dir().join(format!("billo-wall-{}", std::process::id()));
    let d = dir.to_string_lossy().into_owned();
    let r = s.execute("billo.wall.package", json!({"dir": d})).unwrap();
    for f in ["01_MEDIA", "02_AUDIO", "05_TIMELINE", "07_QC"] {
        assert!(dir.join(f).is_dir(), "{f}");
    }
    assert!(dir.join("04_INDEX/shots.json").is_file());
    assert!(dir.join("README.md").is_file());
    assert!(r["files"].as_array().unwrap().len() >= 5);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stems_split_writes_four_stems_under_the_song_in_one_undo_step() {
    let mut s = demo();
    let dir = std::env::temp_dir().join(format!("billo-stems-{}", std::process::id()));
    let (song, _) = first_clip(&s, true);
    let n0 = s.active_sequence().unwrap().audio_tracks.len();
    let undo0 = s.history.undo.len();
    let r = s.execute("billo.stems.split", json!({"song": song, "dir": dir.to_string_lossy()})).unwrap();
    assert_eq!(r["method"], "quick");
    assert_eq!(r["stems"].as_array().unwrap().len(), 4);
    let seq = s.active_sequence().unwrap();
    assert_eq!(seq.audio_tracks.len(), n0 + 4);
    let names: Vec<&str> = seq.audio_tracks.iter().map(|t| t.name.as_str()).collect();
    for want in ["Vocals", "Drums", "Bass", "Other"] {
        assert!(names.contains(&want), "{names:?}");
    }
    let song_item = seq.audio_tracks.iter().flat_map(|t| t.items.iter()).find(|i| i.id.0 == song).unwrap();
    assert!(!song_item.enabled, "the song is muted under its stems");
    let vocals = seq.audio_tracks.iter().find(|t| t.name == "Vocals").unwrap().items.first().unwrap();
    assert_eq!(vocals.start, song_item.start);
    assert_eq!(vocals.source_in, filmcraft_time::Tick::ZERO);
    for f in r["files"].as_array().unwrap() {
        assert!(std::path::Path::new(f["path"].as_str().unwrap()).is_file());
    }
    assert_eq!(s.history.undo.len(), undo0 + 1, "one undo step");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().audio_tracks.len(), n0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stems_split_needs_a_song() {
    let mut s = demo();
    s.state.selection.clear();
    assert!(s.execute("billo.stems.split", json!({})).is_err());
    assert!(s.execute("billo.stems.split", json!({"song": 987_654_321u64})).is_err());
}

#[test]
fn swap_take_keeps_the_slot_and_flags_review() {
    let mut s = demo();
    let seq = s.active_sequence().unwrap();
    let clips: Vec<_> = seq.video_tracks.iter().flat_map(|t| t.items.iter()).map(|i| (i.id.0, i.item.0, i.start, i.duration)).collect();
    let (clip, item, start, duration) = clips[0];
    let other = clips.iter().find(|c| c.1 != item).unwrap().1;
    s.execute("billo.shot.swapTake", json!({"clip": clip, "item": other})).unwrap();
    let seq = s.active_sequence().unwrap();
    let it = seq.video_tracks.iter().flat_map(|t| t.items.iter()).find(|i| i.id.0 == clip).unwrap();
    assert_eq!(it.item.0, other);
    assert_eq!((it.start, it.duration), (start, duration));
    assert_eq!(it.label, filmcraft_project::Label::Yellow);
    assert!(s.execute("billo.shot.swapTake", json!({"clip": clip, "item": 999_999_999u64})).is_err());
}

#[test]
fn versions_number_up_and_list() {
    let mut s = demo();
    let a = s.execute("billo.version.save", json!({"label": "rough cut"})).unwrap();
    let b = s.execute("billo.version.save", json!({})).unwrap();
    assert_eq!(a["version"], 1);
    assert_eq!(b["version"], 2);
    assert!(a["name"].as_str().unwrap().ends_with(" · v1 rough cut"));
    let list = s.execute("billo.version.list", json!({})).unwrap();
    assert_eq!(list["versions"].as_array().unwrap().len(), 2);
    let payload = s.execute("billo.version.payload", json!({"label": "x"})).unwrap();
    assert_eq!(payload["editor"], "Billo Cut");
    assert!(payload["tracks"].as_array().unwrap().len() >= 2);
    assert!(payload["durationSeconds"].as_f64().unwrap() > 0.0);
}

#[test]
fn regen_request_flags_the_shot_and_returns_a_480p_ticket() {
    let mut s = demo();
    let (clip, _) = first_clip(&s, false);
    let r = s.execute("billo.regen.request", json!({"clips": [clip], "prompt": "slow push in, night"})).unwrap();
    assert_eq!(r["tickets"][0]["resolution"], "480p");
    assert_eq!(s.execute("billo.shot.lane", json!({})).unwrap()["counts"]["regen"], 1);
    assert!(s.execute("billo.regen.request", json!({"clips": [clip], "prompt": "  "})).is_err());
    let (audio, _) = first_clip(&s, true);
    assert!(s.execute("billo.regen.request", json!({"clips": [audio], "prompt": "x"})).is_err());
}

#[test]
fn assistant_routes_plain_language() {
    let mut s = demo();
    let dry = |s: &mut Session, text: &str| s.execute("billo.assistant.run", json!({"text": text, "dryRun": true})).unwrap();
    assert_eq!(dry(&mut s, "split the stems on this song")["command"], "billo.stems.split");
    assert_eq!(dry(&mut s, "Solo the VOCALS")["params"]["name"], "Vocals");
    let loud = dry(&mut s, "deliver at -16 LUFS");
    assert_eq!(loud["command"], "billo.deliver.loudness");
    assert_eq!(loud["params"]["target"], -16.0);
    assert_eq!(dry(&mut s, "save version rough cut")["params"]["label"], "rough cut");
    assert_eq!(dry(&mut s, "do my taxes")["understood"], false);
    let (clip, _) = first_clip(&s, false);
    s.state.selection = vec![filmcraft_project::ClipId(clip)];
    let ran = s.execute("billo.assistant.run", json!({"text": "mark this locked"})).unwrap();
    assert_eq!(ran["ran"], true);
    assert_eq!(s.execute("billo.shot.lane", json!({})).unwrap()["counts"]["locked"], 1);
    // a request that cannot run reports why instead of failing
    s.state.selection.clear();
    let failed = s.execute("billo.assistant.run", json!({"text": "mark regen"})).unwrap();
    assert_eq!(failed["ran"], false);
    assert!(failed["error"].is_string());
    assert!(s.execute("billo.assistant.run", json!({"text": ""})).is_err());
}

#[test]
fn every_assistant_request_names_a_real_command() {
    let mut s = Session::default();
    let list = s.execute("command.list", json!({})).unwrap();
    let ids: Vec<String> = list.as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap().to_string()).collect();
    let catalog = s.execute("billo.assistant.catalog", json!({})).unwrap();
    assert!(catalog["count"].as_u64().unwrap() >= 25);
    for r in catalog["requests"].as_array().unwrap() {
        let id = r["command"].as_str().unwrap();
        assert!(ids.iter().any(|c| c == id), "assistant names a missing command: {id}");
    }
}
