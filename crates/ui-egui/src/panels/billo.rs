//! Billo panel: the Billo Zord workflow in one place. Shots (status lane), Stems, Sound,
//! Versions, Ask Billo and the FX Vault. Every button runs a `billo.*` command, so the panel, the
//! Billo menu, MCP and the control channel share one code path.
//!
//! Automation ids: `billo.mark.<locked|real|regen|review>`, `billo.shot.<clip id>`,
//! `billo.stems.split`, `billo.stems.solo.<Vocals|Drums|Bass|Other>`, `billo.stems.clear`,
//! `billo.beats.detect`, `billo.beats.snap`, `billo.cut.silence`, `billo.sound.clean`, `billo.sound.deliver`, `billo.upscale.shot`, `billo.upscale.master`, `billo.version.save`, `billo.ask.text`,
//! `billo.ask.go`, `billo.vault.<all|transitions|luts|text|vocal|sfx|plugins>`.

use egui::{Color32, Rect, RichText, vec2};
use filmcraft_engine::billo as engine;
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::theme::Tokens;

/// Most rows a list shows (the lane of a long cut, the vault).
const MAX_ROWS: usize = 400;
const VAULT_TABS: [&str; 7] = ["all", "transitions", "luts", "text", "vocal", "sfx", "plugins"];

fn status_color(status: &str, t: &Tokens) -> Color32 {
    match status {
        "locked" => Color32::from_rgb(0x4c, 0xa8, 0x5a),
        "real" => Color32::from_rgb(0x4a, 0x8f, 0xe0),
        "regen" => Color32::from_rgb(0xe0, 0x5a, 0x7a),
        "review" => Color32::from_rgb(0xe0, 0xc0, 0x3a),
        _ => t.text_faint,
    }
}

fn heading(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(text).font(Tokens::ui(12.5)).color(t.text).strong());
    ui.add_space(2.0);
}

fn dim(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    ui.label(RichText::new(text).font(Tokens::ui(11.5)).color(t.text_dim));
}

/// A button that queues `cmd` when clicked and registers its automation id.
fn button(ui: &mut egui::Ui, auto: &mut crate::automation::Registry, actions: &mut Vec<(String, Value)>, id: &str, label: &str, cmd: &str, params: Value) {
    let r = ui.button(RichText::new(label).font(Tokens::ui(11.5)));
    auto.add_response(id, &r, label);
    if r.clicked() {
        actions.push((cmd.to_string(), params));
    }
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    ui.painter().rect_filled(rect, 0.0, t.panel_bg);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(12.0, 6.0))));
    let ui = &mut child;
    let mut actions: Vec<(String, Value)> = Vec::new();

    // what the panel shows, read once per frame
    let (shots, stem_tracks, base) = match app.session.active_sequence() {
        Some(seq) => {
            let mut shots: Vec<(u64, String, &'static str, f64)> = seq
                .video_tracks
                .iter()
                .flat_map(|tr| tr.items.iter())
                .map(|i| (i.id.0, i.name.clone(), engine::label_status(i.label).unwrap_or("unmarked"), i.start.seconds()))
                .collect();
            shots.sort_by(|a, b| a.3.total_cmp(&b.3));
            let stems: Vec<String> =
                seq.audio_tracks.iter().map(|tr| tr.name.clone()).filter(|n| ["Vocals", "Drums", "Bass", "Other"].contains(&n.as_str())).collect();
            let name = app.session.state.active_sequence.and_then(|id| app.session.project.item(id)).map(|i| i.name.clone()).unwrap_or_default();
            (shots, stems, engine::version_base(&name).to_string())
        }
        None => (Vec::new(), Vec::new(), String::new()),
    };
    let versions = if base.is_empty() { Vec::new() } else { engine::versions_of(&app.session, &base) };
    let has_seq = app.session.active_sequence().is_some();

    egui::ScrollArea::vertical().id_salt("billo-panel-scroll").auto_shrink([false, false]).show(ui, |ui| {
        if !has_seq {
            dim(ui, &t, tl!("Open a sequence to use the Billo tools."));
        }

        // ---- Ask Billo
        heading(ui, &t, tl!("Ask Billo"));
        let ask_id = egui::Id::new("billo-ask-text");
        let answer_id = egui::Id::new("billo-ask-answer");
        let mut text = ui.data(|d| d.get_temp::<String>(ask_id)).unwrap_or_default();
        let mut go = false;
        ui.horizontal(|ui| {
            let width = (ui.available_width() - 48.0).max(60.0);
            let r = ui.add(
                egui::TextEdit::singleline(&mut text).id_salt("billo-ask").hint_text(tl!("split the stems, solo vocals, mark locked…")).desired_width(width),
            );
            app.auto.add_response("billo.ask.text", &r, &text);
            go = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let b = ui.button(RichText::new(tl!("Go")).font(Tokens::ui(11.5)));
            app.auto.add_response("billo.ask.go", &b, "Go");
            go |= b.clicked();
        });
        if go && !text.trim().is_empty() {
            let answer = match app.session.execute("billo.assistant.run", json!({"text": text})) {
                Ok(r) if r["understood"] == false => tl!("Not on my list yet. Try: split the stems, solo vocals, mark locked, clean dialogue.").to_string(),
                Ok(r) if r["ran"] == true => r["says"].as_str().unwrap_or_default().to_string(),
                Ok(r) => r["error"].as_str().unwrap_or_default().to_string(),
                Err(e) => e.to_string(),
            };
            ui.data_mut(|d| d.insert_temp(answer_id, answer));
            text.clear();
        }
        ui.data_mut(|d| d.insert_temp(ask_id, text));
        let answer = ui.data(|d| d.get_temp::<String>(answer_id)).unwrap_or_default();
        if !answer.is_empty() {
            dim(ui, &t, &answer);
        }

        // ---- Shots
        heading(ui, &t, tl!("Shots"));
        let locked = shots.iter().filter(|s| s.2 == "locked").count();
        let percent = locked.saturating_mul(100).checked_div(shots.len()).unwrap_or(0);
        dim(ui, &t, &tlf!("{locked} of {total} locked ({percent}%)", locked = locked, total = shots.len(), percent = percent));
        ui.horizontal_wrapped(|ui| {
            for (status, cmd) in
                [("locked", "billo.shot.markLocked"), ("real", "billo.shot.markReal"), ("regen", "billo.shot.markRegen"), ("review", "billo.shot.markReview")]
            {
                let label = crate::i18n::t(match status {
                    "locked" => "Locked",
                    "real" => "Real Footage",
                    "regen" => "Needs Regen",
                    _ => "Review",
                });
                let r = ui.button(RichText::new(label).font(Tokens::ui(11.5)).color(status_color(status, &t)));
                app.auto.add_response(&format!("billo.mark.{status}"), &r, status);
                if r.clicked() {
                    actions.push((cmd.to_string(), json!({})));
                }
            }
        });
        // ---- Stems
        heading(ui, &t, tl!("Stems"));
        dim(ui, &t, tl!("Select the song, split it, then work each stem on its own track."));
        ui.horizontal_wrapped(|ui| {
            button(ui, &mut app.auto, &mut actions, "billo.stems.split", crate::i18n::t("Split Song Into Stems"), "billo.stems.splitSelected", json!({}));
            for name in &stem_tracks {
                button(
                    ui,
                    &mut app.auto,
                    &mut actions,
                    &format!("billo.stems.solo.{name}"),
                    &tlf!("Solo {stem}", stem = name),
                    "billo.stems.solo",
                    json!({"name": name}),
                );
            }
            if !stem_tracks.is_empty() {
                button(ui, &mut app.auto, &mut actions, "billo.stems.clear", crate::i18n::t("Clear Stem Solo"), "billo.stems.clearSolo", json!({}));
            }
        });

        // ---- Rhythm
        heading(ui, &t, tl!("Rhythm"));
        dim(ui, &t, tl!("Mark the kicks in the song, then roll every cut onto the nearest one."));
        ui.horizontal_wrapped(|ui| {
            button(ui, &mut app.auto, &mut actions, "billo.beats.detect", crate::i18n::t("Find the Beats in Song"), "billo.beats.detectSelected", json!({}));
            button(ui, &mut app.auto, &mut actions, "billo.beats.snap", crate::i18n::t("Snap Cuts to the Beat"), "billo.beats.snapAll", json!({}));
            button(ui, &mut app.auto, &mut actions, "billo.cut.silence", crate::i18n::t("Cut Dead Air"), "billo.cut.silenceSelected", json!({}));
        });

        // ---- Sound
        heading(ui, &t, tl!("Sound"));
        ui.horizontal_wrapped(|ui| {
            button(ui, &mut app.auto, &mut actions, "billo.sound.clean", crate::i18n::t("Clean Up Dialogue"), "billo.dialogue.cleanSelected", json!({}));
            button(ui, &mut app.auto, &mut actions, "billo.sound.deliver", crate::i18n::t("Deliver at −14 LUFS"), "billo.deliver.loudness14", json!({}));
        });

        // ---- Upscale
        heading(ui, &t, tl!("Upscale"));
        dim(ui, &t, tl!("Queues the job for the upscale runner. Drafts stay at 480p until the end."));
        ui.horizontal_wrapped(|ui| {
            button(ui, &mut app.auto, &mut actions, "billo.upscale.shot", crate::i18n::t("Upscale Shot to 4K"), "billo.upscale.selected4k", json!({}));
            button(ui, &mut app.auto, &mut actions, "billo.upscale.master", crate::i18n::t("Master in 4K"), "billo.upscale.master4k", json!({}));
        });

        // ---- Versions
        heading(ui, &t, tl!("Versions"));
        ui.horizontal_wrapped(|ui| {
            button(ui, &mut app.auto, &mut actions, "billo.version.save", crate::i18n::t("Save Version"), "billo.version.saveNow", json!({}));
        });
        for (_, name, _) in versions.iter().rev().take(12) {
            dim(ui, &t, name);
        }

        // ---- the shot list (below the tools, so a long cut never pushes them out of reach)
        ui.add_space(8.0);
        ui.separator();
        for (clip, name, status, seconds) in shots.iter().take(MAX_ROWS) {
            let selected = app.session.state.selection.iter().any(|c| c.0 == *clip);
            ui.horizontal(|ui| {
                ui.label(RichText::new("●").font(Tokens::ui(11.5)).color(status_color(status, &t)));
                let text = RichText::new(name.as_str()).font(Tokens::ui(11.5)).color(if selected { t.text } else { t.text_dim });
                let r = ui.selectable_label(selected, text);
                app.auto.add_response(&format!("billo.shot.{clip}"), &r, &format!("{name} · {status}"));
                if r.clicked() {
                    actions.push(("timeline.select".into(), json!({"clips": [clip]})));
                    actions.push(("playhead.set".into(), json!({"seconds": seconds})));
                }
                ui.label(RichText::new(crate::i18n::t(status_title(status))).font(Tokens::ui(10.5)).color(t.text_faint));
            });
        }

        // ---- FX Vault
        heading(ui, &t, tl!("FX Vault"));
        let tab_id = egui::Id::new("billo-vault-tab");
        let mut tab = ui.data(|d| d.get_temp::<usize>(tab_id)).unwrap_or(0).min(VAULT_TABS.len().saturating_sub(1));
        ui.horizontal_wrapped(|ui| {
            for (i, name) in VAULT_TABS.iter().enumerate() {
                let r = ui.selectable_label(i == tab, RichText::new(vault_tab_title(name)).font(Tokens::ui(11.0)));
                app.auto.add_response(&format!("billo.vault.{name}"), &r, name);
                if r.clicked() {
                    tab = i;
                }
            }
        });
        ui.data_mut(|d| d.insert_temp(tab_id, tab));
        let category = VAULT_TABS.get(tab).copied().unwrap_or("all");
        for item in engine::fx_vault().iter().filter(|v| category == "all" || v["category"].as_str() == Some(category)).take(MAX_ROWS) {
            let status = item["status"].as_str().unwrap_or_default();
            let (mark, color) = match status {
                "vault" => ("●", Color32::from_rgb(0x4c, 0xa8, 0x5a)),
                "resend" => ("●", Color32::from_rgb(0xe0, 0x9a, 0x3a)),
                "store" => ("●", Color32::from_rgb(0x9a, 0x7a, 0xe0)),
                _ => ("●", Color32::from_rgb(0x4a, 0x8f, 0xe0)),
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(mark).font(Tokens::ui(11.5)).color(color));
                ui.label(RichText::new(item["name"].as_str().unwrap_or_default()).font(Tokens::ui(11.5)).color(t.text_dim));
                ui.label(RichText::new(vault_status_title(status)).font(Tokens::ui(10.5)).color(t.text_faint));
            });
        }
        ui.add_space(12.0);
    });

    for (cmd, params) in actions {
        if let Err(e) = app.session.execute(&cmd, params) {
            app.ui.status = e.to_string();
        }
    }
}

fn status_title(status: &str) -> &'static str {
    match status {
        "locked" => "Locked",
        "real" => "Real Footage",
        "regen" => "Needs Regen",
        "review" => "Review",
        _ => "Unmarked",
    }
}

fn vault_tab_title(name: &str) -> String {
    match name {
        "transitions" => tl!("Video FX").to_string(),
        "luts" => tl!("LUTs").to_string(),
        "text" => tl!("Text").to_string(),
        "vocal" => tl!("Vocal Chains").to_string(),
        "sfx" => tl!("Sound FX").to_string(),
        "plugins" => tl!("Plugins").to_string(),
        _ => tl!("All").to_string(),
    }
}

fn vault_status_title(status: &str) -> String {
    match status {
        "vault" => tl!("in the vault").to_string(),
        "resend" => tl!("re-send needed").to_string(),
        "store" => tl!("in a store account").to_string(),
        _ => tl!("licensed").to_string(),
    }
}
