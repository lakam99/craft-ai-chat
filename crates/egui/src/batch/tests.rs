use super::*;
mod history;
use crate::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct Editor {
    info: HostInfo,
    instance: Arc<()>,
    names: Vec<String>,
    undo: Vec<Vec<String>>,
    profile: Option<CredentialProfile>,
    document: String,
    fail_at: Option<usize>,
    hold: bool,
    remember_history: bool,
}
impl Default for Editor {
    fn default() -> Self {
        Self {
            info: core::editor::EditorTools {
                name: "editor",
                methods: &["engine.execute"],
                views: &[],
            }
            .host_info("editor", "Editor", "", "Attach project context"),
            instance: Arc::new(()),
            names: (0..64).map(|i| format!("Original {i}")).collect(),
            undo: vec![],
            profile: None,
            document: "project-a".into(),
            fail_at: None,
            hold: false,
            remember_history: true,
        }
    }
}
impl Host for Editor {
    type Tool = Value;
    fn info(&self) -> &HostInfo {
        &self.info
    }
    fn instance(&self) -> Arc<()> {
        self.instance.clone()
    }
    fn preferences(&self) -> Preferences {
        Preferences {
            attach_frames: false,
            remember_key: false,
            remember_history: self.remember_history,
            ..Default::default()
        }
    }
    fn credential_profile(&self) -> Option<CredentialProfile> {
        self.profile.clone()
    }
    fn document_key(&self) -> Option<String> {
        Some(self.document.clone())
    }
    fn start_tool(&mut self, call: &ToolCall) -> Result<Value, String> {
        let value: Value = serde_json::from_str(
            call.arguments["params_json"]
                .as_str()
                .ok_or("missing params")?,
        )
        .map_err(|e| e.to_string())?;
        let id = value["params"]["id"].as_u64().ok_or("missing id")? as usize;
        if self.fail_at == Some(id) {
            return Ok(json!({"ok":false,"error":"synthetic rename failure"}));
        }
        let name = value["params"]["name"].as_str().ok_or("missing name")?;
        self.undo.push(self.names.clone());
        *self.names.get_mut(id).ok_or("unknown item")? = name.into();
        Ok(json!({"ok":true,"result":null}))
    }
    fn poll_tool(&mut self, value: &mut Value) -> Result<Option<Value>, String> {
        if self.hold {
            Ok(None)
        } else {
            Ok(Some(value.take()))
        }
    }
    fn set_preference(&mut self, _: &str, _: Value) -> Result<(), String> {
        Ok(())
    }
    fn open_preferences(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn widget(&mut self, _: &str, _: &egui::Response, _: &str) {}
    fn area(&mut self, _: &str, _: egui::Rect, _: &str) {}
}

#[test]
fn reenabling_history_saves_the_current_chat_without_another_message() {
    let dir = tempfile::tempdir().unwrap();
    let mut host = Editor {
        profile: Some(CredentialProfile {
            preferences_file: dir.path().join("prefs.json"),
            service: "test-only".into(),
        }),
        ..Default::default()
    };
    let ctx = egui::Context::default();
    let mut panel = Panel::<Value>::default();
    let deadline = Instant::now() + Duration::from_secs(8);
    panel.poll(&mut host, &ctx);
    while panel.archive.loading() && Instant::now() < deadline {
        panel.poll(&mut host, &ctx);
        std::thread::sleep(Duration::from_millis(2));
    }
    *panel.prompt_mut() = "Keep this conversation when I enable history again".into();
    panel.send_with(&mut host, &ctx, |_| Ok(message())).unwrap();
    finish(&mut panel, &mut host);
    host.remember_history = false;
    panel.poll(&mut host, &ctx);
    assert!(panel.context_items() > 0);
    host.remember_history = true;
    panel.poll(&mut host, &ctx);
    while panel.archive.loading() && Instant::now() < deadline {
        panel.poll(&mut host, &ctx);
        std::thread::sleep(Duration::from_millis(2));
    }
    drop(panel);
    let mut reopened = Panel::<Value>::default();
    reopened.poll(&mut host, &ctx);
    while reopened.archive.loading() && Instant::now() < deadline {
        reopened.poll(&mut host, &ctx);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        reopened.context_items() > 0,
        "enabling saving must persist the existing chat without needing another send"
    );
}
fn args(count: usize) -> Value {
    json!({"editor":"editor","label":"Rename media","commands":(0..count).map(|i|json!({"command":"item.rename","params_json":json!({"id":i,"name":format!("Take {i:02}")}).to_string()})).collect::<Vec<_>>()})
}
fn function(name: &str, args: Value) -> Value {
    json!({"status":"completed","output":[{"type":"function_call","call_id":"batch","name":name,"arguments":args.to_string()}]})
}
fn message() -> Value {
    json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Finished requested work."}]}]})
}
fn finish(panel: &mut Panel<Value>, host: &mut Editor) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let ctx = egui::Context::default();
    while panel.busy() && Instant::now() < deadline {
        panel.poll(host, &ctx);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!panel.busy(), "{}", panel.message());
}
#[test]
fn sixty_four_renames_use_one_tool_call_one_card_and_keep_undo() {
    let mut panel = Panel::default();
    let mut host = Editor::default();
    *panel.prompt_mut() = "Rename 64 media items".into();
    let requests = Arc::new(AtomicUsize::new(0));
    let calls = requests.clone();
    panel
        .send_with(&mut host, &egui::Context::default(), move |body| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(function("batch_commands", args(64)))
            } else {
                let result: Value = serde_json::from_str(
                    body["input"].as_array().unwrap().last().unwrap()["output"]
                        .as_str()
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(result["completed"], 64);
                assert_eq!(result["remaining"], 0);
                Ok(message())
            }
        })
        .unwrap();
    finish(&mut panel, &mut host);
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    assert_eq!(host.names[63], "Take 63");
    assert_eq!(host.undo.len(), 64);
    assert_eq!(
        panel
            .transcript()
            .iter()
            .filter(|(role, _)| role == "Tool")
            .count(),
        1
    );
    assert!(
        panel
            .transcript()
            .iter()
            .any(|(_, s)| s.contains("64 of 64 commands completed"))
    );
    host.names = host.undo.pop().unwrap();
    assert_eq!(host.names[63], "Original 63");
    if let Some(dir) = std::env::var_os("CRAFT_AI_EDITOR_FIXTURES") {
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(520.0, 760.0))
            .build_ui_state(
                |ui, (panel, host): &mut (Panel<Value>, Editor)| {
                    panel.show(host, ui, ui.max_rect())
                },
                (panel, host),
            );
        h.run_steps(4);
        std::fs::create_dir_all(&dir).unwrap();
        h.render()
            .unwrap()
            .save(std::path::Path::new(&dir).join("batch-history-preview.png"))
            .unwrap();
    }
}
#[test]
fn batch_stops_at_first_editor_error_and_returns_completed_count() {
    let mut host = Editor {
        fail_at: Some(2),
        ..Default::default()
    };
    let mut batch = Batch::new(&host.info, &args(5)).unwrap();
    assert!(batch.poll(&mut host).unwrap().is_none());
    assert!(batch.poll(&mut host).unwrap().is_none());
    let result = batch.poll(&mut host).unwrap().unwrap();
    assert_eq!(result["ok"], false);
    assert_eq!(result["completed"], 2);
    assert_eq!(
        result["results"][2]["result"]["error"],
        "synthetic rename failure"
    );
    assert_eq!(host.names[3], "Original 3");
}
#[test]
fn hostile_batch_envelopes_are_rejected_before_any_edit() {
    let host = Editor::default();
    for count in [0, 65] {
        assert!(Batch::<Value>::new(&host.info, &args(count)).is_err());
    }
    let mut bad = args(3);
    bad["commands"][2]["params_json"] = json!("[]");
    assert!(Batch::<Value>::new(&host.info, &bad).is_err());
    for command in ["ai.send", "ai.preferences", "batch_commands"] {
        let mut bad = args(3);
        bad["commands"][2]["command"] = json!(command);
        assert!(Batch::<Value>::new(&host.info, &bad).is_err());
    }
    let mut bad = args(3);
    bad["commands"][2]["params_json"] =
        json!(json!({"key":"ai.model","value":"wrong"}).to_string());
    assert!(Batch::<Value>::new(&host.info, &bad).is_err());
    let mut bad = args(3);
    bad["editor"] = json!("shell");
    assert!(Batch::<Value>::new(&host.info, &bad).is_err());
}
#[test]
fn stopping_a_batch_or_replacing_document_prevents_later_edits() {
    for replace in [false, true] {
        let mut panel = Panel::default();
        let mut host = Editor::default();
        *panel.prompt_mut() = "Rename files".into();
        panel
            .send_with(&mut host, &egui::Context::default(), move |_| {
                Ok(function("batch_commands", args(64)))
            })
            .unwrap();
        let ctx = egui::Context::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        while host.undo.len() < 3 && Instant::now() < deadline {
            panel.poll(&mut host, &ctx);
            std::thread::sleep(Duration::from_millis(1));
        }
        let done = host.undo.len();
        assert!((3..64).contains(&done));
        if replace {
            host.instance = Arc::new(());
        } else {
            panel.stop();
        }
        finish(&mut panel, &mut host);
        assert_eq!(host.undo.len(), done);
    }
}
#[test]
fn tool_limit_and_network_failure_preserve_original_request_for_continue() {
    for network in [false, true] {
        let mut panel = Panel::default();
        let mut host = Editor::default();
        *panel.prompt_mut() = "Rename all media to Take NN, preserving extensions".into();
        let mut count = 0usize;
        panel.send_with(&mut host,&egui::Context::default(),move |_|{
            count+=1;if network && count==3{return Err("synthetic network failure".into());}
            Ok(function("editor",json!({"method":"engine.execute","params_json":json!({"command":"item.rename","params":{"id":count-1,"name":format!("Take {}",count)}}).to_string()})))
        }).unwrap();
        finish(&mut panel, &mut host);
        assert_eq!(host.undo.len(), if network { 2 } else { 32 });
        assert!(panel.context_items() > 0);
        *panel.prompt_mut() = "continue".into();
        panel
            .send_with(&mut host, &egui::Context::default(), move |body| {
                let text = body["input"].to_string();
                assert!(text.contains("preserving extensions"));
                assert!(text.contains("item.rename"));
                assert!(text.contains("continue"));
                Ok(message())
            })
            .unwrap();
        finish(&mut panel, &mut host);
    }
}
#[test]
fn local_history_restores_after_restart_is_scoped_and_new_chat_clears_it() {
    let dir = tempfile::tempdir().unwrap();
    let profile = CredentialProfile {
        preferences_file: dir.path().join("prefs.json"),
        service: "test-only".into(),
    };
    let mut host = Editor {
        profile: Some(profile),
        ..Default::default()
    };
    let ctx = egui::Context::default();
    let mut panel = Panel::<Value>::default();
    let deadline = Instant::now() + Duration::from_secs(8);
    panel.poll(&mut host, &ctx);
    while panel.archive.loading() && Instant::now() < deadline {
        panel.poll(&mut host, &ctx);
        std::thread::sleep(Duration::from_millis(2));
    }
    panel
        .set_session_key("sk-private-test-key-that-must-not-be-saved".into())
        .unwrap();
    *panel.prompt_mut() =
        "Remember my naming rule sk-private-test-key-that-must-not-be-saved".into();
    panel.send_with(&mut host, &ctx, |_| Ok(message())).unwrap();
    finish(&mut panel, &mut host);
    let folder = dir.path().join("ai-chat-history");
    while (!folder.is_dir()
        || std::fs::read_dir(&folder)
            .unwrap()
            .filter_map(Result::ok)
            .all(|e| e.path().extension().is_none_or(|s| s != "json")))
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(2));
    }
    drop(panel);
    let mut restored = Panel::<Value>::default();
    restored.poll(&mut host, &ctx);
    while restored.archive.loading() && Instant::now() < deadline {
        restored.poll(&mut host, &ctx);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        restored
            .transcript()
            .iter()
            .any(|(_, s)| s.contains("naming rule"))
    );
    assert!(!restored.has_key());
    assert!(
        !serde_json::to_string(&restored.history)
            .unwrap()
            .contains("sk-private-test-key")
    );
    assert!(restored.context_items() > 0);
    host.document = "project-b".into();
    host.instance = Arc::new(());
    restored.poll(&mut host, &ctx);
    while restored.archive.loading() && Instant::now() < deadline {
        restored.poll(&mut host, &ctx);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(restored.transcript().is_empty());
    assert_eq!(restored.context_items(), 0);
    host.document = "project-a".into();
    host.instance = Arc::new(());
    restored.poll(&mut host, &ctx);
    while restored.archive.loading() && Instant::now() < deadline {
        restored.poll(&mut host, &ctx);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(restored.context_items() > 0);
    restored
        .command(&mut host, &ctx, "ai.clear")
        .unwrap()
        .unwrap();
    assert_eq!(restored.context_items(), 0);
    drop(restored);
    let mut cleared = Panel::<Value>::default();
    cleared.poll(&mut host, &ctx);
    while cleared.archive.loading() && Instant::now() < deadline {
        cleared.poll(&mut host, &ctx);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(cleared.transcript().is_empty());
    assert_eq!(cleared.context_items(), 0);
}
