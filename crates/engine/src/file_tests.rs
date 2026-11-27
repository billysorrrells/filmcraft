//! `.fcproj` save/open through the command interface: schema envelope, legacy upgrade, Save a Copy,
//! Revert.

use super::*;
use serde_json::json;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("fc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn saved_file_is_versioned_and_save_copy_keeps_state() {
    let d = temp_dir("schema");
    let path = d.join("p.fcproj").to_string_lossy().to_string();
    let mut s = demo();
    s.execute("file.saveAs", json!({"path": path})).unwrap();
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["schema_version"], filmcraft_format::SCHEMA_VERSION);
    assert_eq!(v["format"], "filmcraft.project");
    s.execute("sequence.addEdit", json!({"seconds": 1.0})).unwrap();
    assert!(s.is_dirty());
    let copy = d.join("copy.fcproj").to_string_lossy().to_string();
    s.execute("file.saveCopy", json!({"path": copy})).unwrap();
    assert!(s.is_dirty(), "Save a Copy leaves the project unsaved");
    assert_eq!(s.path.as_deref(), Some(path.as_str()));
    let mut t = Session::default();
    t.execute("file.open", json!({"path": copy})).unwrap();
    assert_eq!(*t.project, *s.project);
    // Revert discards the edit.
    s.execute("file.revert", json!({})).unwrap();
    assert!(!s.is_dirty());
    assert_ne!(*t.project, *s.project);
    assert!(!Session::default().is_enabled("file.revert"), "nothing to revert to before a save");
    // No temp files left next to the project.
    let stray: Vec<_> = std::fs::read_dir(&d).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).collect();
    assert!(stray.is_empty());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn legacy_project_opens_and_first_save_keeps_a_backup() {
    let d = temp_dir("legacy");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../format/tests/fixtures/v1-edit.fcproj");
    let path = d.join("old.fcproj");
    std::fs::copy(&fixture, &path).unwrap();
    let p = path.to_string_lossy().to_string();
    let mut s = Session::default();
    let r = s.execute("file.open", json!({"path": p})).unwrap();
    assert_eq!(r["schemaVersion"], 1);
    assert_eq!(r["migrated"], true);
    assert_eq!(s.project.name, "Legacy Edit");
    assert!(s.drain_events().iter().any(|e| matches!(e, Event::Toast { message, .. } if message.contains("Upgraded"))));
    let r = s.execute("file.save", json!({})).unwrap();
    let backup = r["backup"].as_str().unwrap().to_string();
    assert!(backup.ends_with("old (schema v1 backup).fcproj"), "{backup}");
    assert_eq!(std::fs::read(&backup).unwrap(), std::fs::read(&fixture).unwrap());
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["schema_version"], filmcraft_format::SCHEMA_VERSION);
    // Second save: no new backup.
    assert!(s.execute("file.save", json!({})).unwrap()["backup"].is_null());
    // A file from the future is refused clearly.
    let mut v = v;
    v["schema_version"] = json!(filmcraft_format::SCHEMA_VERSION + 1);
    std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    let e = Session::default().execute("file.open", json!({"path": p})).unwrap_err().to_string();
    assert!(e.contains("newer version of FilmCraft"), "{e}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn project_is_named_after_its_file() {
    let d = temp_dir("name");
    let mut s = demo();
    let a = d.join("Apollo 11 - Tranquility.fcproj").to_string_lossy().to_string();
    s.execute("file.saveAs", json!({"path": a})).unwrap();
    assert_eq!(s.project.name, "Apollo 11 - Tranquility");
    assert_eq!(s.project.root.name, "Apollo 11 - Tranquility");
    assert!(!s.is_dirty(), "renaming on Save As is not an edit");
    // Save a Copy keeps the current name.
    let c = d.join("Backup copy.fcproj").to_string_lossy().to_string();
    s.execute("file.saveCopy", json!({"path": c})).unwrap();
    assert_eq!(s.project.name, "Apollo 11 - Tranquility");
    s.execute("file.open", json!({"path": c})).unwrap();
    assert_eq!(s.project.name, "Apollo 11 - Tranquility");
    let _ = std::fs::remove_dir_all(&d);
}

/// Opening the demo after a saved project must not keep that path: Save would overwrite it.
#[test]
fn open_demo_project_clears_saved_path_so_save_does_not_overwrite() {
    let d = temp_dir("demo-path");
    let original = d.join("original.fcproj").to_string_lossy().to_string();
    let mut s = Session::default();
    s.execute("file.newProject", json!({"name": "Original"})).unwrap();
    s.execute("file.newBin", json!({"name": "ORIGINAL"})).unwrap();
    s.execute("file.saveAs", json!({"path": original})).unwrap();
    s.execute("file.openDemoProject", json!({})).unwrap();
    assert!(s.path.is_none(), "demo is unsaved, like New Project");
    let err = s.execute("file.save", json!({})).unwrap_err().to_string();
    assert!(err.contains("no path") || err.contains("Save As"), "{err}");
    let mut reopen = Session::default();
    reopen.execute("file.open", json!({"path": original})).unwrap();
    assert!(
        reopen.project.root.children.iter().any(|e| matches!(e, filmcraft_project::BinEntry::Bin(b) if b.name == "ORIGINAL")),
        "original.fcproj still has the ORIGINAL bin"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// File ▸ Open on a Premiere `.prproj`: a new unsaved project with its bins, media and sequence, and
/// the sequence opens in the timeline. Saving writes a `.fcproj`, never the `.prproj`.
#[test]
fn premiere_project_opens_as_a_new_unsaved_project() {
    const PRPROJ: &str = r#"<?xml version="1.0" encoding="UTF-8" ?>
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
  <Media ObjectUID="m1"><VideoStream ObjectRef="12"/><FilePath>/nowhere/take1.mov</FilePath><Title>take1.mov</Title></Media>
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
  <VideoClipTrackItem ObjectID="40"><ClipTrackItem><TrackItem><Start>0</Start><End>508032000000</End></TrackItem><SubClip ObjectRef="41"/></ClipTrackItem></VideoClipTrackItem>
  <SubClip ObjectID="41"><Clip ObjectRef="42"/><MasterClip ObjectURef="mc1"/><Name>Take 1</Name></SubClip>
  <VideoClip ObjectID="42"><Clip><Source ObjectRef="11"/><InPoint>0</InPoint></Clip></VideoClip>
</PremiereData>"#;
    let d = temp_dir("prproj");
    let path = d.join("Show Edit.prproj").to_string_lossy().to_string();
    std::fs::write(&path, PRPROJ).unwrap();
    let mut s = Session::default();
    let r = s.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(r["premiere"], true);
    assert_eq!(s.project.name, "Show Edit");
    assert!(s.path.is_none(), "the .prproj is never the save target");
    let seqs: Vec<_> = s.project.items.values().filter(|i| i.as_sequence().is_some()).collect();
    assert_eq!(seqs.len(), 1);
    let seq = seqs[0].as_sequence().unwrap();
    assert_eq!(seq.tracks(filmcraft_project::TrackKind::Video)[0].items.len(), 1);
    let _ = std::fs::remove_dir_all(&d);
}
