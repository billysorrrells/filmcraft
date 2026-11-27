//! Headless UI tests of the Billo panel: it opens beside the Program monitor, its buttons run the
//! `billo.*` commands on the selection, the shot list selects clips, and Ask Billo routes a request.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn counts(&mut self) -> Value {
        self.exec("billo.shot.lane", json!({}))["counts"].clone()
    }
}

#[test]
fn panel_opens_and_marks_the_selected_shot() {
    let mut d = Driver::demo();
    d.exec("window.panel.Billo", json!({}));
    d.frames(3);
    assert!(!d.ids("program.").is_empty(), "Program monitor still shown");
    let ids = d.ids("billo.");
    for id in [
        "billo.mark.locked",
        "billo.mark.regen",
        "billo.stems.split",
        "billo.sound.clean",
        "billo.sound.deliver",
        "billo.upscale.master",
        "billo.version.save",
        "billo.ask.text",
        "billo.ask.go",
        "billo.vault.luts",
    ] {
        assert!(ids.iter().any(|i| i == id), "missing {id} in {ids:?}");
    }
    // the shot list selects a clip; Locked marks it
    let shot = ids.iter().find(|i| i.starts_with("billo.shot.")).expect("a shot row").clone();
    d.click(&shot);
    assert_eq!(d.counts()["locked"], 0);
    d.click("billo.mark.locked");
    assert_eq!(d.counts()["locked"], 1);
    d.click("billo.mark.regen");
    assert_eq!(d.counts()["regen"], 1);
    assert_eq!(d.counts()["locked"], 0);
    // Save Version keeps a numbered copy
    d.click("billo.version.save");
    assert_eq!(d.exec("billo.version.list", json!({}))["versions"].as_array().unwrap().len(), 1);
}

#[test]
fn ask_billo_runs_a_request() {
    let mut d = Driver::demo();
    d.exec("window.panel.Billo", json!({}));
    d.frames(3);
    let shot = d.ids("billo.shot.").first().expect("a shot row").clone();
    d.click(&shot);
    d.click("billo.ask.text");
    d.ok("ui.type", json!({"text": "mark this locked"}));
    d.frames(2);
    d.click("billo.ask.go");
    assert_eq!(d.counts()["locked"], 1);
}
