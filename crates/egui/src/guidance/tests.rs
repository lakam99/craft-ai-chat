use super::*;
use crate::{HostInfo, Preferences, ToolCall};
use egui::{Context, vec2};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

struct Editor {
    info: HostInfo,
    instance: Arc<()>,
    widgets: HashMap<String, Rect>,
    edits: usize,
    escaped: bool,
}
impl Default for Editor {
    fn default() -> Self {
        let info = super::super::core::editor::EditorTools {
            name: "editor",
            methods: &["ui.elements"],
            views: &[],
        }
        .host_info("editor", "Editor", "", "Attach editor context");
        Self {
            info: super::super::core::editor::with_guidance(info),
            instance: Arc::new(()),
            widgets: [(
                "toolbar.play".into(),
                Rect::from_min_size(pos2(70.0, 80.0), vec2(80.0, 30.0)),
            )]
            .into(),
            edits: 0,
            escaped: false,
        }
    }
}
impl Host for Editor {
    type Tool = ();
    fn info(&self) -> &HostInfo {
        &self.info
    }
    fn instance(&self) -> Arc<()> {
        self.instance.clone()
    }
    fn preferences(&self) -> Preferences {
        Preferences {
            remember_key: false,
            ..Default::default()
        }
    }
    fn highlight_target(&self, id: &str) -> Option<HighlightTarget> {
        self.widgets.get(id).copied().map(HighlightTarget::new)
    }
    fn start_tool(&mut self, _: &ToolCall) -> Result<(), String> {
        self.edits += 1;
        Err("Guidance must not dispatch an editor action".into())
    }
    fn poll_tool(&mut self, _: &mut ()) -> Result<Option<Value>, String> {
        Ok(None)
    }
    fn set_preference(&mut self, _: &str, _: Value) -> Result<(), String> {
        Err("No preferences during guidance".into())
    }
    fn open_preferences(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn widget(&mut self, id: &str, response: &egui::Response, _: &str) {
        self.widgets.insert(id.into(), response.interact_rect);
    }
    fn area(&mut self, id: &str, rect: Rect, _: &str) {
        self.widgets.insert(id.into(), rect);
    }
}
fn request(id: &str) -> Value {
    json!({"targets":[{"id":id,"label":"Press Play"}]})
}

#[test]
fn hostile_or_missing_targets_do_not_replace_a_valid_guide() {
    let host = Editor::default();
    let mut guide = Guidance::default();
    guide.apply(&host, &request("toolbar.play")).unwrap();
    for args in [
        request("missing"),
        request("ai.send"),
        request("settings.ai.key"),
        json!({"targets":[{"id":"toolbar.play","label":"Play","x":1}]}),
        json!({"targets":[{"id":"toolbar.play","label":"Play"},{"id":"toolbar.play","label":"Duplicate"}]}),
        json!({"targets":vec![json!({"id":"toolbar.play","label":"Play"});9]}),
        json!({"targets":[{"id":"toolbar.play","label":"x".repeat(161)}]}),
    ] {
        assert!(guide.apply(&host, &args).is_err(), "{args}");
        assert_eq!(guide.targets[0].id, "toolbar.play");
    }
    guide.apply(&host, &json!({"targets":[]})).unwrap();
    assert!(guide.targets.is_empty());
}

#[test]
fn a_how_to_turn_highlights_without_dispatching_an_edit() {
    let mut host = Editor::default();
    let mut panel = Panel::<()>::default();
    *panel.prompt_mut() = "How do I play my video?".into();
    let mut calls = 0;
    panel.send_with(&mut host, &Context::default(), move |body| {
        calls += 1;
        assert!(body["instructions"].as_str().unwrap().contains("guide them instead of performing the edit"));
        if calls == 1 {
            Ok(json!({"status":"completed","output":[{"type":"function_call","name":"highlight_ui","call_id":"guide","arguments":request("toolbar.play").to_string()}]}))
        } else {
            assert!(body["input"].to_string().contains("highlighted"));
            Ok(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"1. Click the highlighted Play button."}]}]}))
        }
    }).unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    while panel.busy() && Instant::now() < deadline {
        panel.poll(&mut host, &Context::default());
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!panel.busy());
    assert_eq!(host.edits, 0);
    assert_eq!(panel.guidance.targets.len(), 1);
    assert!(
        panel
            .transcript()
            .iter()
            .any(|(_, t)| t == "Highlight editor controls — Done")
    );
    host.instance = Arc::new(());
    panel.poll(&mut host, &Context::default());
    assert!(
        panel.guidance.targets.is_empty(),
        "a new document cannot inherit the old guide"
    );
}

#[test]
fn outlines_follow_live_geometry_and_hide_removed_controls() {
    let mut host = Editor::default();
    let mut panel = Panel::<()>::default();
    panel
        .guidance
        .apply(&host, &request("toolbar.play"))
        .unwrap();
    let ctx = Context::default();
    for x in [70.0, 240.0] {
        host.widgets.insert(
            "toolbar.play".into(),
            Rect::from_min_size(pos2(x, 80.0), vec2(80.0, 30.0)),
        );
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(500.0, 300.0))),
                ..Default::default()
            },
            |ui| panel.paint_guidance(&host, ui.ctx()),
        );
        output.textures_delta.clear();
        assert!(
            output
                .shapes
                .iter()
                .any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.rect.left() == x - 3.0))
        );
    }
    host.widgets.clear();
    let mut output = ctx.run_ui(Default::default(), |ui| {
        panel.paint_guidance(&host, ui.ctx())
    });
    output.textures_delta.clear();
    assert!(
        output.shapes.is_empty(),
        "closed controls cannot leave an outline at stale coordinates"
    );
    assert_eq!(host.edits, 0);
    for size in [vec2(0.0, 0.0), vec2(8.0, 8.0)] {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), size)),
                ..Default::default()
            },
            |ui| panel.paint_guidance(&host, ui.ctx()),
        );
        output.textures_delta.clear();
    }
}

#[test]
fn highlighted_buttons_remain_clickable_and_dismissal_is_respected() {
    use egui_kittest::{Harness, kittest::Queryable};
    let mut harness = Harness::builder()
        .with_size(vec2(960.0, 640.0))
        .with_theme(egui::Theme::Dark)
        .build_ui_state(
            |ui, (panel, host, clicks): &mut (Panel<()>, Editor, usize)| {
                egui::Panel::right("chat")
                    .exact_size(360.0)
                    .show(ui, |ui| panel.show(host, ui, ui.max_rect()));
                ui.heading("Editor");
                ui.label("The numbered controls are ready for you to use.");
                ui.add_space(24.0);
                for (id, label) in [("toolbar.play", "Play"), ("setup.inputs", "Audio Inputs")] {
                    let response = ui.button(label);
                    host.widgets.insert(id.into(), response.interact_rect);
                    if response.clicked() {
                        *clicks += 1;
                    }
                }
                if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                    host.escaped = true;
                }
                panel.paint_guidance(host, ui.ctx());
            },
            (Panel::default(), Editor::default(), 0),
        );
    harness.run_steps(3);
    let (panel, host, _) = harness.state_mut();
    panel.guidance.apply(host, &json!({"targets":[{"id":"toolbar.play","label":"Click Play to start playback"},{"id":"setup.inputs","label":"Open Audio Inputs to map your device"}]})).unwrap();
    panel.record("Assistant", "1. Click the highlighted **Play** button.\n2. Open **Audio Inputs** to configure your channels.".into());
    harness.run_steps(3);
    if let Some(path) = std::env::var_os("CRAFT_AI_GUIDANCE_FIXTURE") {
        harness.render().unwrap().save(path).unwrap();
    }
    harness
        .query_by_role_and_label(egui::accesskit::Role::Button, "Play")
        .unwrap()
        .click();
    harness.run_steps(3);
    assert_eq!(
        harness.state().2,
        1,
        "an outline must not consume the underlying button click"
    );
    harness.get_by_label("Clear highlights").click();
    harness.run_steps(3);
    let (panel, host, _) = harness.state_mut();
    assert!(panel.guidance.targets.is_empty());
    assert!(
        panel
            .guidance
            .apply(host, &request("toolbar.play"))
            .is_err(),
        "the current turn cannot undo the user's dismissal"
    );
    panel.guidance.begin_turn();
    panel
        .guidance
        .apply(host, &request("toolbar.play"))
        .unwrap();
    harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.run_steps(3);
    assert!(
        harness.state().1.escaped,
        "the editor must still receive Escape"
    );
    assert!(
        harness.state().0.guidance.targets.is_empty(),
        "guidance also dismisses after the editor consumes Escape"
    );
}

#[test]
fn a_long_guide_keeps_the_composer_and_dismissal_inside_a_small_dock() {
    use egui_kittest::Harness;
    let mut host = Editor::default();
    let targets: Vec<Value> = (0..8).map(|n| {
        let id = format!("editor.control.{n}");
        host.widgets.insert(id.clone(), Rect::from_min_size(pos2(30.0, 30.0), vec2(20.0, 20.0)));
        json!({"id":id,"label":"A long instruction that wraps across several lines in a narrow editor dock"})
    }).collect();
    let mut panel = Panel::<()>::default();
    panel
        .guidance
        .apply(&host, &json!({"targets":targets}))
        .unwrap();
    let mut h = Harness::builder()
        .with_size(vec2(220.0, 490.0))
        .build_ui_state(
            |ui, (panel, host): &mut (Panel<()>, Editor)| panel.show(host, ui, ui.max_rect()),
            (panel, host),
        );
    h.run_steps(4);
    for id in ["ai.prompt", "ai.send", "ai.clearHighlights"] {
        let rect = h.state().1.widgets.get(id).unwrap();
        assert!(
            rect.min.x >= 0.0 && rect.min.y >= 0.0 && rect.max.x <= 220.0 && rect.max.y <= 490.0,
            "clipped {id}: {rect:?}"
        );
    }
}

#[test]
fn guide_card_waits_for_completed_layout_before_marking_a_control_hidden() {
    use egui_kittest::{Harness, kittest::Queryable};
    let host = Editor::default();
    let mut panel = Panel::<()>::default();
    panel
        .guidance
        .apply(&host, &request("toolbar.play"))
        .unwrap();
    let mut h = Harness::builder()
        .with_size(vec2(400.0, 300.0))
        .build_ui_state(
            |ui, (panel, host, finished): &mut (Panel<()>, Editor, bool)| {
                host.widgets.clear(); // The target panel has not drawn yet.
                panel.guidance_ui(host, ui);
                if *finished {
                    panel.paint_guidance(host, ui.ctx());
                }
            },
            (panel, host, false),
        );
    h.run_steps(3);
    assert!(h.query_by_label("1. Press Play (control hidden)").is_none());
    h.state_mut().2 = true; // The completed pass really no longer contains this control.
    h.run_steps(3);
    h.get_by_label("1. Press Play (control hidden)");
}
