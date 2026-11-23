use super::*;
use crate::core::goal::Phase;
fn loaded(panel: &mut Panel<Value>, host: &mut Editor, ctx: &egui::Context) {
    panel.poll(host, ctx);
    let deadline = Instant::now() + Duration::from_secs(5);
    while panel.archive.loading() && Instant::now() < deadline {
        panel.poll(host, ctx);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!panel.archive.loading(), "{}", panel.archive.message);
}
fn host(dir: &std::path::Path) -> Editor {
    let mut host = Editor {
        profile: Some(CredentialProfile {
            preferences_file: dir.join("prefs.json"),
            service: "test-only".into(),
        }),
        ..Default::default()
    };
    host.info = core::editor::with_guidance(host.info);
    host
}
fn response(name: &str, id: &str, args: Value) -> Value {
    let mut value = function(name, args);
    value["output"][0]["call_id"] = json!(id);
    value
}
#[test]
fn interruption_drains_current_batch_command_and_steers_without_replaying() {
    let dir = tempfile::tempdir().unwrap();
    let mut host = host(dir.path());
    let ctx = egui::Context::default();
    let mut panel = Panel::default();
    loaded(&mut panel, &mut host, &ctx);
    host.hold = true;
    panel.prompt = "Rename four items".into();
    let requests = Arc::new(AtomicUsize::new(0));
    let count = requests.clone();
    panel
        .send_with(&mut host, &ctx, move |body| {
            let n = count.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(function("batch_commands", args(4)))
            } else {
                let text = body["input"].to_string();
                assert!(text.contains("Leave the remaining items unchanged"));
                let result = body["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|i| i["type"] == "function_call_output")
                    .unwrap()["output"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let result: Value = serde_json::from_str(&result).unwrap();
                assert_eq!(result["completed"], 1);
                assert_eq!(result["remaining"], 3);
                Ok(message())
            }
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while host.undo.is_empty() && Instant::now() < deadline {
        panel.poll(&mut host, &ctx);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(host.undo.len(), 1);
    panel.prompt = "Leave the remaining items unchanged".into();
    panel
        .send_with(&mut host, &ctx, |_| {
            panic!("must retain the original transport for queued steering")
        })
        .unwrap();
    assert!(panel.prompt.is_empty());
    host.hold = false;
    finish(&mut panel, &mut host);
    assert_eq!(host.undo.len(), 1);
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    assert!(panel.queued.is_empty());
    assert_eq!(
        panel
            .transcript()
            .iter()
            .filter(|(role, _)| role == "You")
            .count(),
        2
    );
}
#[test]
fn new_chat_retains_old_conversation_and_goals_restore_paused() {
    let dir = tempfile::tempdir().unwrap();
    let mut host = host(dir.path());
    let ctx = egui::Context::default();
    let mut panel = Panel::default();
    loaded(&mut panel, &mut host, &ctx);
    panel.goal_mode = true;
    panel.prompt = "Make a bright brass sound; ask me to listen before finishing.".into();
    panel
        .send_with(&mut host, &ctx, |_| {
            Ok(response(
                "goal_wait",
                "wait-1",
                json!({"reason":"Please listen and describe the remaining change."}),
            ))
        })
        .unwrap();
    finish(&mut panel, &mut host);
    assert_eq!(
        crate::goals::lock(panel.goal.as_ref().unwrap()).goal.phase,
        Phase::NeedsInput
    );
    let old = panel.archive.library.as_ref().unwrap().active().to_owned();
    panel.switch_chat(&host, &ctx, None).unwrap();
    loaded(&mut panel, &mut host, &ctx);
    assert!(panel.transcript().is_empty());
    panel.prompt = "A separate conversation".into();
    panel.send_with(&mut host, &ctx, |_| Ok(message())).unwrap();
    finish(&mut panel, &mut host);
    drop(panel);
    let mut reopened = Panel::default();
    loaded(&mut reopened, &mut host, &ctx);
    assert!(
        reopened
            .transcript()
            .iter()
            .any(|(_, text)| text == "A separate conversation")
    );
    reopened.switch_chat(&host, &ctx, Some(&old)).unwrap();
    loaded(&mut reopened, &mut host, &ctx);
    let session = crate::goals::lock(reopened.goal.as_ref().unwrap());
    assert_eq!(session.goal.phase, Phase::Paused);
    assert!(session.goal.instructions[0].contains("bright brass"));
    assert!(!reopened.busy());
    assert!(host.undo.is_empty());
    drop(session);
    if let Some(path) = std::env::var_os("CRAFT_AI_EDITOR_FIXTURES") {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(500.0, 760.0))
            .build_ui_state(
                |ui, (panel, host): &mut (Panel<Value>, Editor)| {
                    panel.show(host, ui, ui.max_rect());
                },
                (reopened, host),
            );
        harness.run_steps(4);
        std::fs::create_dir_all(&path).unwrap();
        harness
            .render()
            .unwrap()
            .save(std::path::Path::new(&path).join("goal-history-preview.png"))
            .unwrap();
    }
}
#[test]
fn continuous_goal_crosses_tool_budget_and_verifies_fresh_readback() {
    let dir = tempfile::tempdir().unwrap();
    let mut host = host(dir.path());
    let ctx = egui::Context::default();
    let mut panel = Panel::default();
    loaded(&mut panel, &mut host, &ctx);
    panel.goal_mode = true;
    panel.prompt = "Rename the first 33 items to Take 00 through Take 32.".into();
    let mut operations = 0;
    panel.send_with(&mut host,&ctx,move|body|{
        if body["tools"].as_array().unwrap().is_empty(){
            if body["instructions"].as_str().unwrap().starts_with("Independently verify"){
                assert!(body["input"].to_string().contains("freshEditorReadback"));
                let mut value=message();value["output"][0]["content"][0]["text"]=json!("{\"verified\":true,\"reason\":\"All requested names are present in saved edit receipts.\"}");return Ok(value);
            }return Ok(message());
        }
        if body["tool_choice"]=="none"{return Ok(message());}
        assert!(body["input"].to_string().contains("Rename the first 33 items"));
        if operations<33{let id=operations;operations+=1;
            Ok(response("editor",&format!("rename-{id}"),json!({"method":"engine.execute","params_json":json!({"command":"item.rename","params":{"id":id,"name":format!("Take {id:02}")}}).to_string()})))
        }else{Ok(response("goal_complete","complete",json!({"summary":"Renamed 33 items."})))}
    }).unwrap();
    finish(&mut panel, &mut host);
    assert_eq!(host.undo.len(), 33);
    assert_eq!(host.names[32], "Take 32");
    let session = crate::goals::lock(panel.goal.as_ref().unwrap());
    assert_eq!(session.goal.phase, Phase::Complete, "{}", panel.message());
    assert_eq!(session.goal.completed, 34);
}
#[test]
fn failed_summary_recovers_exact_goal_and_receipts_without_repeating_edits() {
    let dir = tempfile::tempdir().unwrap();
    let mut host = host(dir.path());
    let ctx = egui::Context::default();
    let mut panel = Panel::default();
    loaded(&mut panel, &mut host, &ctx);
    panel.goal_mode = true;
    panel.prompt = "Preserve names and extensions; wait for my approval.".into();
    panel.history = (0..130)
        .map(|i| json!({"role":"user","content":format!("Historical note {i}")}))
        .collect();
    panel
        .send_with(&mut host, &ctx, |body| {
            if body["tools"].as_array().unwrap().is_empty() {
                return Ok(json!({"status":"incomplete"}));
            }
            assert!(
                body["input"]
                    .to_string()
                    .contains("Preserve names and extensions")
            );
            Ok(response(
                "goal_wait",
                "wait",
                json!({"reason":"Waiting for your approval."}),
            ))
        })
        .unwrap();
    finish(&mut panel, &mut host);
    let session = crate::goals::lock(panel.goal.as_ref().unwrap());
    assert_eq!(session.goal.recovery_count, 1);
    assert_eq!(session.goal.phase, Phase::NeedsInput);
    assert!(host.undo.is_empty());
}
#[test]
fn queued_corrections_survive_restart_without_automatic_edits() {
    let dir = tempfile::tempdir().unwrap();
    let mut host = host(dir.path());
    let ctx = egui::Context::default();
    let mut panel = Panel::default();
    loaded(&mut panel, &mut host, &ctx);
    host.hold = true;
    panel.prompt = "Rename four items".into();
    panel
        .send_with(&mut host, &ctx, |_| Ok(function("batch_commands", args(4))))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while host.undo.is_empty() && Instant::now() < deadline {
        panel.poll(&mut host, &ctx);
        std::thread::sleep(Duration::from_millis(1));
    }
    panel.prompt = "Leave all remaining items unchanged".into();
    panel
        .send_with(&mut host, &ctx, |_| panic!("queued only"))
        .unwrap();
    panel.stop();
    host.hold = false;
    finish(&mut panel, &mut host);
    assert_eq!(host.undo.len(), 1);
    drop(panel);
    let mut reopened = Panel::default();
    loaded(&mut reopened, &mut host, &ctx);
    assert_eq!(reopened.queued, vec!["Leave all remaining items unchanged"]);
    assert!(!reopened.resume_queued);
    assert!(!reopened.busy());
}
