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
