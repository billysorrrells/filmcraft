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

/// A Render Wall timeline shaped like the live ones (`billo-timeline/1`).
fn wall_fixture() -> serde_json::Value {
    json!({
        "schema": "billo-timeline/1", "slug": "test-cut", "title": "TEST · Billo Zord", "fps": 24, "frames": 144, "duration": 6.0, "canvas": [854, 480],
        "bin": {"black": {"kind": "black"}, "A2": {"media": "a2.mp4"}, "B1": {"media": "b1.mp4"}},
        "tracks": [
            {"id": "V2", "name": "Story · status", "kind": "gfx", "editable": false, "clips": [
                {"id": "s01", "f0": 0, "n": 48, "label": "THE HOUSE · LOCKED kit"},
                {"id": "s02", "f0": 72, "n": 48, "label": "MEMORIES · old kit, regen"}]},
            {"id": "V1", "name": "Picture", "kind": "video", "editable": true, "clips": [
                {"id": "c01", "src": "A2", "ss": 0.5, "n": 48, "lut": null, "label": "THE HOUSE", "f0": 0, "thumb": "t1.jpg"},
                {"id": "g1", "src": "black", "ss": 0.0, "n": 24, "label": "Black", "f0": 48},
                {"id": "c02", "src": "B1", "ss": 1.0, "n": 48, "lut": null, "label": "MEMORIES", "f0": 72, "thumb": "t2.jpg"},
                {"id": "c03", "src": "MISSING", "ss": 0.0, "n": 24, "label": "LOST", "f0": 120}]},
            {"id": "A1", "name": "Record · full mix", "kind": "audio", "url": "rec.m4a"},
            {"id": "A2", "name": "Vocals", "kind": "audio", "url": "voc.m4a"}
        ]
    })
}

fn wall_items(s: &Session) -> serde_json::Value {
    let seq = s.active_sequence().unwrap();
    let mut video: Vec<u64> = seq.video_tracks.iter().flat_map(|t| t.items.iter()).map(|i| i.item.0).collect();
    video.dedup();
    let audio = seq.audio_tracks.iter().flat_map(|t| t.items.iter()).last().unwrap().item.0;
    json!({"A2": video[0], "B1": video.iter().find(|v| **v != video[0]).unwrap(), "track:A1": audio, "track:A2": audio})
}

#[test]
fn wall_open_builds_the_sequence_and_round_trips() {
    let mut s = demo();
    let items = wall_items(&s);
    let undo0 = s.history.undo.len();
    let r = s.execute("billo.wall.open", json!({"timeline": wall_fixture(), "items": items})).unwrap();
    assert_eq!(r["placed"], 2);
    assert_eq!(r["skipped"], json!(["MISSING"]));
    assert_eq!(s.history.undo.len(), undo0 + 1, "one undo step");
    let seq = s.active_sequence().unwrap();
    assert_eq!((seq.settings.width, seq.settings.height), (854, 480));
    let rate = seq.settings.frame_rate;
    let v1 = &seq.video_tracks[0].items;
    assert_eq!(v1.len(), 2);
    assert_eq!((v1[0].name.as_str(), rate.frame_at(v1[0].start), rate.frame_at(v1[0].end())), ("THE HOUSE", 0, 48));
    assert_eq!((v1[1].name.as_str(), rate.frame_at(v1[1].start)), ("MEMORIES", 72));
    assert!((v1[0].source_in.seconds() - 0.5).abs() < 1e-6);
    assert_eq!(v1[0].label, filmcraft_project::Label::Forest, "LOCKED block paints the shot");
    assert_eq!(v1[1].label, filmcraft_project::Label::Rose, "regen block paints the shot");
    assert_eq!(seq.audio_tracks.len(), 2);
    assert_eq!(seq.audio_tracks[0].name, "Record · full mix");
    assert!(!seq.audio_tracks[0].muted && seq.audio_tracks[1].muted, "the record plays, the stem waits muted");
    assert!(seq.video_tracks.iter().chain(seq.audio_tracks.iter()).all(|t| t.items.iter().all(|i| i.id.0 != 0)));

    // unchanged cut -> the same picture clips come back, ids and thumbs kept, the hole is black
    let back = s.execute("billo.wall.timeline", json!({"base": wall_fixture(), "items": items})).unwrap();
    let clips = back["timeline"]["tracks"][1]["clips"].as_array().unwrap();
    let brief: Vec<_> =
        clips.iter().map(|c| (c["id"].as_str().unwrap(), c["src"].as_str().unwrap(), c["f0"].as_i64().unwrap(), c["n"].as_i64().unwrap())).collect();
    assert_eq!(brief, vec![("c01", "A2", 0, 48), ("g1", "black", 48, 24), ("c02", "B1", 72, 48)]);
    assert_eq!(clips[0]["thumb"], "t1.jpg");
    assert_eq!(clips[0]["ss"], 0.5);
    assert_eq!(clips[0]["status"], "locked");
    assert_eq!(back["timeline"]["frames"], 120);
    assert_eq!(back["timeline"]["tracks"][0]["clips"].as_array().unwrap().len(), 2, "other tracks are left alone");
    assert_eq!(back["unmapped"], json!([]));
    s.execute("edit.undo", json!({})).unwrap();
}

#[test]
fn wall_commands_reject_bad_input() {
    let mut s = demo();
    assert!(s.execute("billo.wall.open", json!({})).is_err());
    assert!(s.execute("billo.wall.open", json!({"timeline": {"fps": 24}})).is_err());
    assert!(s.execute("billo.wall.timeline", json!({})).is_err());
    assert!(s.execute("billo.wall.timeline", json!({"base": {"tracks": []}})).is_err());
    // hostile numbers do not panic
    let odd = json!({"fps": 1e30, "canvas": [0, 99999999u64], "tracks": [{"kind": "video", "clips": [{"src": "A2", "f0": -5, "n": 1e300, "ss": -1e300}]}]});
    let items = wall_items(&s);
    let _ = s.execute("billo.wall.open", json!({"timeline": odd, "items": items}));
}

#[test]
fn upscale_request_returns_job_rows() {
    let mut s = demo();
    let (clip, _) = first_clip(&s, false);
    let r = s.execute("billo.upscale.request", json!({"clips": [clip], "target": "4K", "tag": "real"})).unwrap();
    assert_eq!(r["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(r["jobs"][0]["target_res"], "4k");
    assert_eq!(r["jobs"][0]["tag"], "real_footage");
    assert_eq!(r["jobs"][0]["passes"].as_array().unwrap().len(), 3);
    let all = s.execute("billo.upscale.request", json!({"timeline": true})).unwrap();
    assert!(all["jobs"].as_array().unwrap().len() > 1);
    assert!(s.execute("billo.upscale.request", json!({"clips": [clip], "target": "8k"})).is_err());
    s.state.selection.clear();
    assert!(s.execute("billo.upscale.request", json!({})).is_err());
}

/// Import a generated mono signal as a stereo WAV and return its project item.
fn import_signal(s: &mut Session, name: &str, mono: &[f32]) -> u64 {
    let stereo: Vec<f32> = mono.iter().flat_map(|v| [*v, *v]).collect();
    let bytes = crate::previews::write_wav_f32(&stereo, 48_000);
    crate::commands::import_bytes(s, &format!("/billo-test/{name}.wav"), bytes.into(), None).unwrap().0
}

fn audio_clip_of(s: &Session, item: u64) -> u64 {
    s.active_sequence().unwrap().audio_tracks.iter().flat_map(|t| t.items.iter()).find(|i| i.item.0 == item).unwrap().id.0
}

#[test]
fn beats_are_marked_and_cuts_roll_onto_them() {
    let mut s = demo();
    let (video_a, video_b) = {
        let seq = s.active_sequence().unwrap();
        let v: Vec<u64> = seq.video_tracks.iter().flat_map(|t| t.items.iter()).map(|i| i.item.0).collect();
        (v[0], *v.iter().find(|x| **x != v[0]).unwrap())
    };
    // kicks every half second
    let mut song = vec![0.0f32; 4 * 48_000];
    for k in 1..8 {
        for n in 0..9_600 {
            let t = n as f32 / 48_000.0;
            song[k * 24_000 + n] += (2.0 * std::f32::consts::PI * 60.0 * t).sin() * (-t * 25.0).exp();
        }
    }
    let song_item = import_signal(&mut s, "kicks", &song);
    s.execute("file.newSequence", json!({"name": "beat cut", "fps": 24.0, "video": 1, "audio": 1})).unwrap();
    s.execute("timeline.place", json!({"item": song_item, "audioTrack": "A1", "time": 0, "video": false, "audio": true})).unwrap();
    let rate = s.active_sequence().unwrap().settings.frame_rate;
    let frames = |n: i64| rate.tick_of(n).0;
    // the cut sits 2 frames early of the kick at 1.0 s (frame 24)
    s.execute("timeline.place", json!({"item": video_a, "track": "V1", "time": 0, "duration": frames(22), "video": true, "audio": false})).unwrap();
    s.execute(
        "timeline.place",
        json!({"item": video_b, "track": "V1", "time": frames(22), "sourceIn": frames(24), "duration": frames(30), "video": true, "audio": false}),
    )
    .unwrap();
    let song_clip = audio_clip_of(&s, song_item);

    assert!(s.execute("billo.beats.snapCuts", json!({})).is_err(), "no beats marked yet");
    let found = s.execute("billo.beats.detect", json!({"song": song_clip})).unwrap();
    assert_eq!(found["count"], 7, "{found}");
    assert!((found["bpm"].as_f64().unwrap() - 120.0).abs() < 4.0);
    let beats = s.active_sequence().unwrap().markers.iter().filter(|m| m.name.starts_with("BEAT")).count();
    assert_eq!(beats, 7);
    // a second pass replaces the markers instead of doubling them
    s.execute("billo.beats.detect", json!({"song": song_clip})).unwrap();
    assert_eq!(s.active_sequence().unwrap().markers.iter().filter(|m| m.name.starts_with("BEAT")).count(), 7);

    let r = s.execute("billo.beats.snapCuts", json!({"tolerance": 6})).unwrap();
    assert_eq!((r["cuts"].as_u64(), r["moved"].as_u64(), r["onBeatPercent"].as_u64()), (Some(1), Some(1), Some(100)), "{r}");
    let seq = s.active_sequence().unwrap();
    let v1 = &seq.video_tracks[0].items;
    assert_eq!(rate.frame_at(v1[0].end()), 24);
    assert_eq!(rate.frame_at(v1[1].start), 24);
    let again = s.execute("billo.beats.snapCuts", json!({})).unwrap();
    assert_eq!(again["alreadyOnBeat"], 1);
    assert!(s.execute("billo.beats.detect", json!({"song": song_clip, "band": "nope"})).is_err());
}

#[test]
fn dead_air_is_cut_and_the_cut_closes_up() {
    let mut s = demo();
    let mut talk = vec![0.0f32; 4 * 48_000];
    for (i, v) in talk.iter_mut().enumerate() {
        let t = i as f32 / 48_000.0;
        if !(1.0..2.5).contains(&t) {
            *v = 0.3 * (2.0 * std::f32::consts::PI * 220.0 * t).sin();
        }
    }
    let item = import_signal(&mut s, "talk", &talk);
    s.execute("file.newSequence", json!({"name": "talk", "fps": 24.0, "video": 1, "audio": 1})).unwrap();
    s.execute("timeline.place", json!({"item": item, "audioTrack": "A1", "time": 0, "video": false, "audio": true})).unwrap();
    let clip = audio_clip_of(&s, item);
    let end = |s: &Session| s.active_sequence().unwrap().audio_tracks[0].items.iter().map(|i| i.end().seconds()).fold(0.0, f64::max);
    assert!((end(&s) - 4.0).abs() < 0.05);

    let dry = s.execute("billo.cut.silence", json!({"clip": clip, "dryRun": true})).unwrap();
    assert_eq!((dry["found"].as_u64(), dry["removed"].as_u64()), (Some(1), Some(0)));
    assert!((end(&s) - 4.0).abs() < 0.05, "a dry run changes nothing");

    let r = s.execute("billo.cut.silence", json!({"clip": clip})).unwrap();
    assert_eq!(r["removed"], 1);
    let removed = r["seconds"].as_f64().unwrap();
    assert!((removed - 1.26).abs() < 0.1, "1.5 s of silence less 0.12 s kept each side: {removed}");
    assert_eq!(s.active_sequence().unwrap().audio_tracks[0].items.len(), 2, "the clip is split around the hole");
    assert!((end(&s) - (4.0 - removed)).abs() < 0.05, "the cut closed up");
    s.execute("edit.undo", json!({})).unwrap();
    assert!((end(&s) - 4.0).abs() < 0.05);
    // nothing quiet enough: nothing removed
    let none = s.execute("billo.cut.silence", json!({"clip": clip, "minSilence": 5.0})).unwrap();
    assert_eq!(none["found"], 0);
    assert!(s.execute("billo.cut.silence", json!({"clip": 424_242u64})).is_err());
}
