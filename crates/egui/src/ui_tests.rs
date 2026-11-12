use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use std::collections::HashMap;

struct Preview {
    info: HostInfo,
    instance: Arc<()>,
    widgets: HashMap<String, egui::Rect>,
}
impl Default for Preview {
    fn default() -> Self {
        Self {
            info: HostInfo {
                id: "preview".into(),
                name: "Craft Editor".into(),
                instructions: String::new(),
                tools: vec![],
                automatic_inputs: vec![],
                attachment_label: "Attach current views".into(),
                welcome: "Work with your editor through a conversation.".into(),
            },
            instance: Arc::new(()),
            widgets: HashMap::new(),
        }
    }
}
impl Host for Preview {
    type Tool = ();
    fn info(&self) -> &HostInfo {
        &self.info
    }
    fn preferences(&self) -> Preferences {
        Preferences::default()
    }
    fn instance(&self) -> Arc<()> {
        self.instance.clone()
    }
    fn start_tool(&mut self, _: &ToolCall) -> Result<(), String> {
        Err("No tools in the offline preview".into())
    }
    fn poll_tool(&mut self, _: &mut ()) -> Result<Option<Value>, String> {
        Ok(None)
    }
    fn set_preference(&mut self, _: &str, _: Value) -> Result<(), String> {
        Ok(())
    }
    fn open_preferences(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn widget(&mut self, id: &str, response: &egui::Response, _: &str) {
        self.widgets.insert(id.into(), response.rect);
    }
    fn area(&mut self, id: &str, rect: egui::Rect, _: &str) {
        self.widgets.insert(id.into(), rect);
    }
}
fn fixture() -> (Panel<()>, Preview) {
    let mut panel = Panel::default();
    panel
        .set_session_key("sk-offline-preview-placeholder".into())
        .unwrap();
    panel.record("You", "Make the opening feel more cinematic.".into());
    let first = panel.chat.begin("Inspect timeline".into());
    panel
        .chat
        .finish(first, ActivityState::Done, "12 clips reviewed".into());
    let second = panel.chat.begin("Apply fade transition".into());
    panel
        .chat
        .finish(second, ActivityState::Done, String::new());
    panel.record("Assistant", "## Opening pass\n\nThe opening is tighter, with a **gentler transition** into the wide shot.\n\n- Trimmed the first shot\n- Added a short fade\n- Kept the original audio\n\nUse **Undo** if you want to compare.\n\n```text\nOpening → Wide shot\n```\n\n| Change | Result |\n| --- | --- |\n| Timing | Tighter |\n| Transition | Softer |".into());
    *panel.prompt_mut() = "Now match the colour of the next shot.".into();
    (panel, Preview::default())
}
fn harness(size: egui::Vec2, populated: bool) -> Harness<'static, (Panel<()>, Preview)> {
    let state = if populated {
        fixture()
    } else {
        (Panel::default(), Preview::default())
    };
    Harness::builder()
        .with_size(size)
        .with_theme(egui::Theme::Dark)
        .build_ui_state(
            |ui, (panel, host)| {
                host.widgets.clear();
                panel.show(host, ui, ui.max_rect());
            },
            state,
        )
}
#[test]
fn composer_and_header_stay_visible_in_a_narrow_dock() {
    for size in [egui::vec2(220.0, 490.0), egui::vec2(340.0, 600.0)] {
        let mut h = harness(size, true);
        h.run_steps(4);
        for id in [
            "ai.prompt",
            "ai.send",
            "ai.clear",
            "ai.attach",
            "ai.settings",
            "ai.connection",
        ] {
            let rect = h.state().1.widgets.get(id).unwrap();
            assert!(
                rect.min.x >= 0.0 && rect.max.x <= size.x + 1.0 && rect.max.y <= size.y + 1.0,
                "{id} {rect:?}"
            );
        }
        assert!(
            !h.state().1.widgets.contains_key("ai.key"),
            "Connection fields must not occupy the conversation by default"
        );
        assert!(
            !h.state().1.widgets.contains_key("ai.stop"),
            "Stop is only shown during a request"
        );
    }
}
#[test]
fn connection_is_explicit_and_forgotten_password_cannot_return_through_undo() {
    let mut h = harness(egui::vec2(340.0, 700.0), false);
    h.run_steps(4);
    h.get_by_label("Set up API key").click();
    h.run_steps(3);
    h.ctx
        .memory_mut(|m| m.request_focus(egui::Id::unique("ai.key.field")));
    h.input_mut()
        .events
        .push(egui::Event::Text("sk-private-test-credential".into()));
    h.run_steps(3);
    assert!(h.state().0.has_key());
    h.get_by_label("Forget key").click();
    h.run_steps(3);
    assert!(!h.state().0.has_key());
    h.ctx
        .memory_mut(|m| m.request_focus(egui::Id::unique("ai.key.field")));
    h.input_mut()
        .events
        .push(egui::Event::ModifiersChanged(egui::Modifiers {
            command: true,
            ..Default::default()
        }));
    h.input_mut().events.push(egui::Event::Key {
        key: egui::Key::Z,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers {
            command: true,
            ..Default::default()
        },
    });
    h.run_steps(3);
    assert!(!h.state().0.has_key());
}
#[test]
fn enter_submits_without_a_newline_and_missing_key_keeps_the_draft() {
    let mut h = harness(egui::vec2(340.0, 700.0), false);
    h.run_steps(3);
    h.ctx
        .memory_mut(|m| m.request_focus(egui::Id::unique("ai.prompt.field")));
    h.input_mut()
        .events
        .push(egui::Event::Text("Inspect my work".into()));
    h.run_steps(2);
    h.input_mut().events.push(egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Default::default(),
    });
    h.run_steps(3);
    assert_eq!(h.state().0.prompt, "Inspect my work");
    assert!(!h.state().0.busy());
    assert!(h.state().0.connection_open);
    assert!(h.state().0.message().contains("valid OpenAI API key"));
}
#[test]
#[ignore = "exports local offline UI previews; requires a working graphics adapter"]
fn export_chat_previews() {
    std::fs::create_dir_all("target/ui-preview").unwrap();
    for (name, size, populated) in [
        ("conversation", egui::vec2(520.0, 820.0), true),
        ("narrow", egui::vec2(220.0, 650.0), true),
        ("setup", egui::vec2(360.0, 720.0), false),
    ] {
        let mut h = harness(size, populated);
        h.run_steps(5);
        h.render()
            .unwrap()
            .save(format!("target/ui-preview/{name}.png"))
            .unwrap();
    }
}

#[test]
fn markdown_is_rendered_and_copy_keeps_the_source() {
    let mut h = harness(egui::vec2(520.0, 820.0), true);
    h.run_steps(4);
    h.get_by_label("Opening pass");
    h.get_by_label("Timing");
    let original = h.state().0.transcript().last().unwrap().1.clone();
    h.get_by_label("Copy").click();
    h.step();
    assert!(
        h.output()
            .platform_output
            .commands
            .iter()
            .any(|command| matches!(command,
        egui::OutputCommand::CopyText(text) if text == &original))
    );
}
#[test]
fn shift_enter_inserts_a_line_without_submitting() {
    let mut h = harness(egui::vec2(340.0, 700.0), false);
    h.run_steps(3);
    h.ctx
        .memory_mut(|m| m.request_focus(egui::Id::unique("ai.prompt.field")));
    h.input_mut()
        .events
        .push(egui::Event::Text("First line".into()));
    h.run_steps(2);
    h.input_mut()
        .events
        .push(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
    h.input_mut().events.push(egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::SHIFT,
    });
    h.run_steps(2);
    assert_eq!(h.state().0.prompt, "First line\n");
    assert!(!h.state().0.busy());
    assert!(!h.state().0.connection_open);
}

#[test]
fn timed_out_actions_fail_and_cancelled_actions_stop() {
    for stopped in [false, true] {
        let mut panel = Panel::<()>::default();
        let mut host = Preview::default();
        let (events_tx, events) = mpsc::channel();
        let (reply, _) = mpsc::channel();
        let activity_id = panel.chat.begin("Inspect editor state".into());
        panel.pending = Some(Pending {
            events,
            cancel: Arc::new(AtomicBool::new(stopped)),
            instance: host.instance(),
            tool: Some(ToolJob {
                batch: None,
                started: false,
                activity_id,
                call: ToolCall::new("inspect", json!({})),
                reply,
                deadline: Instant::now() - Duration::from_secs(1),
                tool: None,
            }),
        });
        panel.poll_inner(&mut host, &egui::Context::default());
        let visible = &panel.transcript()[0].1;
        assert!(
            visible.contains(if stopped {
                "Stopped"
            } else {
                "Failed: Host tool timed out"
            }),
            "{visible}"
        );
        drop(events_tx);
    }
}
