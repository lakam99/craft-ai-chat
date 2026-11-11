use craft_ai_chat_egui::core::function;
use craft_ai_chat_egui::{Host, HostInfo, Panel, Preferences, ToolCall};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

struct Document {
    info: HostInfo,
    prefs: Preferences,
    instance: Arc<()>,
    title: String,
    undo: Vec<String>,
    calls: usize,
    panic_on_edit: bool,
    hold_tool: bool,
    extra_detail: bool,
    thread: std::thread::ThreadId,
    widgets: HashMap<String, egui::Id>,
    rects: HashMap<String, egui::Rect>,
}
impl Document {
    fn new() -> Self {
        Self {
            info: HostInfo {
                id: "document-craft".into(),
                name: "Document Craft".into(),
                instructions: "Use rename_document to change the page title.".into(),
                tools: vec![
                    function(
                        "rename_document",
                        "Rename this document",
                        json!({"type":"object","properties":{"title":{"type":"string"}},"required":["title"],"additionalProperties":false}),
                    ),
                    function(
                        "inspect_page",
                        "Read the active page",
                        json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
                    ),
                ],
                automatic_inputs: vec![ToolCall::new("inspect_page", json!({}))],
                attachment_label: "Attach active page".into(),
                welcome: "Ask about this document.".into(),
            },
            prefs: Preferences::default(),
            instance: Arc::new(()),
            title: "Original".into(),
            undo: vec![],
            calls: 0,
            panic_on_edit: false,
            hold_tool: false,
            extra_detail: false,
            thread: std::thread::current().id(),
            widgets: HashMap::new(),
            rects: HashMap::new(),
        }
    }
}
impl Host for Document {
    type Tool = Option<Value>;
    fn info(&self) -> &HostInfo {
        &self.info
    }
    fn preferences(&self) -> Preferences {
        self.prefs.clone()
    }
    fn instance(&self) -> Arc<()> {
        self.instance.clone()
    }
    fn start_tool(&mut self, call: &ToolCall) -> Result<Self::Tool, String> {
        assert_eq!(
            std::thread::current().id(),
            self.thread,
            "host operations must stay on the UI thread"
        );
        self.calls += 1;
        match call.name.as_str() {
            "inspect_page" => Ok(Some(
                json!([{"type":"input_text","text":format!("Current page: {}",self.title)}]),
            )),
            "rename_document" => {
                assert!(!self.panic_on_edit, "simulated host failure");
                let title = call
                    .arguments
                    .get("title")
                    .and_then(Value::as_str)
                    .filter(|s| s.len() <= 4096)
                    .ok_or("Invalid title")?;
                self.undo.push(self.title.clone());
                self.title = title.into();
                Ok(Some(if self.extra_detail {
                    json!({"ok":true,"debugPayload":"RAW_INTERNAL_RESULT"})
                } else {
                    json!({"ok":true})
                }))
            }
            _ => Err("Unsupported document tool".into()),
        }
    }
    fn poll_tool(&mut self, tool: &mut Self::Tool) -> Result<Option<Value>, String> {
        if self.hold_tool {
            Ok(None)
        } else {
            Ok(tool.take())
        }
    }
    fn set_preference(&mut self, key: &str, value: Value) -> Result<(), String> {
        match key {
            "ai.attachFrames" => {
                self.prefs.attach_frames = value.as_bool().ok_or("Expected a boolean")?
            }
            _ => return Err("Unknown preference".into()),
        }
        Ok(())
    }
    fn open_preferences(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn widget(&mut self, id: &str, response: &egui::Response, _: &str) {
        self.widgets.insert(id.into(), response.id);
        self.rects.insert(id.into(), response.rect);
    }
    fn area(&mut self, _: &str, _: egui::Rect, _: &str) {}
}
fn message() -> Value {
    json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Updated the document."}]}]})
}
fn edit() -> Value {
    json!({"status":"completed","output":[{"type":"function_call","name":"rename_document","call_id":"rename","arguments":"{\"title\":\"Shared plugin edit\"}"}]})
}
fn finish(panel: &mut Panel<Option<Value>>, host: &mut Document) {
    let ctx = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(5);
    while panel.busy() && Instant::now() < deadline {
        panel.poll(host, &ctx);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!panel.busy(), "{}", panel.message());
}

#[test]
fn a_document_host_uses_its_own_tools_and_context_without_video_concepts() {
    let mut host = Document::new();
    let mut panel = Panel::default();
    *panel.prompt_mut() = "Rename the page".into();
    let mut count = 0;
    panel
        .send_with(&mut host, &egui::Context::default(), move |body| {
            count += 1;
            assert!(!body.to_string().contains("video_frame"));
            assert!(!body.to_string().contains("filmcraft"));
            assert!(!body.to_string().contains("effectcraft"));
            assert!(
                body["input"][0]["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|p| p["text"] == "Current page: Original")
            );
            if count == 2 {
                let output = body["input"].as_array().unwrap().last().unwrap()["output"]
                    .as_str()
                    .unwrap();
                assert_eq!(
                    serde_json::from_str::<Value>(output).unwrap(),
                    json!({"ok":true})
                );
            }
            Ok(if count == 1 { edit() } else { message() })
        })
        .unwrap();
    finish(&mut panel, &mut host);
    assert_eq!(host.title, "Shared plugin edit");
    assert_eq!(host.calls, 2);
    host.title = host.undo.pop().unwrap();
    assert_eq!(host.title, "Original");
}

#[test]
fn replacing_a_document_cancels_a_delayed_custom_tool() {
    let mut host = Document::new();
    host.prefs.attach_frames = false;
    let mut panel = Panel::default();
    *panel.prompt_mut() = "Rename".into();
    let (send, receive) = mpsc::channel();
    let (started, ready) = mpsc::channel();
    panel
        .send_with(&mut host, &egui::Context::default(), move |_| {
            started.send(()).unwrap();
            receive.recv_timeout(Duration::from_secs(3)).unwrap();
            Ok(edit())
        })
        .unwrap();
    ready.recv_timeout(Duration::from_secs(3)).unwrap();
    host.instance = Arc::new(());
    panel.poll(&mut host, &egui::Context::default());
    send.send(()).unwrap();
    finish(&mut panel, &mut host);
    assert_eq!(host.calls, 0);
    assert_eq!(host.title, "Original");
    assert_eq!(panel.context_items(), 0);
}

#[test]
fn switching_hosts_clears_conversation_and_session_credentials() {
    let mut host = Document::new();
    let mut panel = Panel::default();
    *panel.prompt_mut() = "Hello".into();
    panel
        .set_session_key("sk-private-test-credential".into())
        .unwrap();
    panel
        .send_with(&mut host, &egui::Context::default(), |body| {
            assert!(!body.to_string().contains("sk-private"));
            Ok(message())
        })
        .unwrap();
    finish(&mut panel, &mut host);
    assert!(panel.has_key());
    assert!(panel.context_items() > 0);
    host.info.id = "another-craft".into();
    panel.poll(&mut host, &egui::Context::default());
    assert!(!panel.has_key());
    assert_eq!(panel.context_items(), 0);
    assert!(panel.transcript().is_empty());
}

#[test]
fn common_panel_preserves_password_forget_and_narrow_layout() {
    use egui_kittest::{Harness, kittest::Queryable};
    let state = (Panel::default(), Document::new());
    let mut h = Harness::builder()
        .with_size(egui::vec2(240.0, 600.0))
        .build_ui_state(
            |ui, (panel, host): &mut (Panel<Option<Value>>, Document)| {
                host.widgets.clear();
                host.rects.clear();
                panel.show(host, ui, ui.max_rect());
            },
            state,
        );
    h.run_steps(4);
    for id in [
        "ai.prompt",
        "ai.send",
        "ai.clear",
        "ai.attach",
        "ai.settings",
    ] {
        let rect = h.state().1.rects.get(id).unwrap();
        assert!(
            rect.min.x >= 0.0 && rect.max.x <= 241.0 && rect.max.y <= 601.0,
            "{id}: {rect:?}"
        );
    }
    h.get_by_label("Set up API key").click();
    h.run_steps(3);
    let field = *h.state().1.widgets.get("ai.key").unwrap();
    h.ctx.memory_mut(|m| m.request_focus(field));
    h.input_mut()
        .events
        .push(egui::Event::Text("sk-private-test-credential".into()));
    h.run_steps(3);
    assert!(h.state().0.has_key());
    h.get_by_label("Forget key").click();
    h.run_steps(3);
    assert!(!h.state().0.has_key());
    h.ctx.memory_mut(|m| m.request_focus(field));
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
    assert!(
        !h.state()
            .0
            .status(&h.state().1)
            .to_string()
            .contains("sk-private")
    );
}

#[test]
fn a_panicking_host_cancels_the_turn_without_running_another_tool() {
    let mut host = Document::new();
    host.prefs.attach_frames = false;
    host.panic_on_edit = true;
    let mut panel = Panel::default();
    *panel.prompt_mut() = "Rename".into();
    panel
        .send_with(&mut host, &egui::Context::default(), |_| Ok(edit()))
        .unwrap();
    finish(&mut panel, &mut host);
    assert_eq!(host.title, "Original");
    assert_eq!(host.calls, 1);
    assert!(panel.message().contains("tool failed"));
    assert_eq!(panel.context_items(), 0);
}

#[test]
fn tool_cards_show_progress_without_leaking_raw_payloads_to_chat() {
    let mut host = Document::new();
    host.prefs.attach_frames = false;
    host.hold_tool = true;
    host.extra_detail = true;
    let mut panel = Panel::default();
    *panel.prompt_mut() = "Rename".into();
    let mut calls = 0;
    panel
        .send_with(&mut host, &egui::Context::default(), move |body| {
            calls += 1;
            if calls == 1 {
                return Ok(edit());
            }
            // Presentation must not change the result returned to the model.
            assert!(body.to_string().contains("RAW_INTERNAL_RESULT"));
            Ok(message())
        })
        .unwrap();
    let ctx = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(3);
    while host.calls == 0 && Instant::now() < deadline {
        panel.poll(&mut host, &ctx);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(host.calls, 1);
    assert!(
        panel
            .transcript()
            .iter()
            .any(|(_, text)| text == "Rename document — Working")
    );
    host.hold_tool = false;
    finish(&mut panel, &mut host);
    assert!(
        panel
            .transcript()
            .iter()
            .any(|(_, text)| text == "Rename document — Done")
    );
    let visible = panel.status(&host).to_string();
    for raw in [
        "RAW_INTERNAL_RESULT",
        "debugPayload",
        "call_id",
        "arguments",
    ] {
        assert!(!visible.contains(raw), "{visible}");
    }
}
