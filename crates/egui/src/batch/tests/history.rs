use super::*;
#[test]
fn repeated_failed_compaction_cannot_grow_context_without_bound() {
    let mut host = Editor::default();
    let mut panel = Panel::<Value> {
        history: (0..512)
            .map(|i| json!({"role":"user","content":format!("Preserved instruction {i}")}))
            .collect(),
        ..Default::default()
    };
    *panel.prompt_mut() = "Keep this draft".into();
    let previous = panel.history.clone();
    for _ in 0..3 {
        assert!(
            panel
                .send_with(&mut host, &egui::Context::default(), |_| panic!(
                    "a full recovery context must reject before another API request"
                ))
                .is_err()
        );
        assert_eq!(panel.history, previous);
        assert_eq!(panel.prompt_mut(), "Keep this draft");
        assert!(!panel.busy());
    }
    assert!(host.undo.is_empty());
}
#[test]
fn restart_preserves_required_reasoning_and_tool_pairs_without_replaying_edits() {
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
    panel.history = vec![
        json!({"role":"user","content":"Keep the original bulk rename rule"}),
        json!({"type":"reasoning","id":"r1","summary":[],"encrypted_content":"opaque-provider-state"}),
        json!({"type":"function_call","id":"f1","call_id":"c1","name":"editor","arguments":"{}"}),
        json!({"type":"function_call_output","call_id":"c1","output":"{\"ok\":true}"}),
    ];
    panel.history_model = core::DEFAULT_MODEL.into();
    panel.persist();
    drop(panel);
    let mut reopened = Panel::<Value>::default();
    reopened.poll(&mut host, &ctx);
    while reopened.archive.loading() && Instant::now() < deadline {
        reopened.poll(&mut host, &ctx);
        std::thread::sleep(Duration::from_millis(2));
    }
    *reopened.prompt_mut() = "continue".into();
    reopened
        .send_with(&mut host, &ctx, |body| {
            let input = body["input"].as_array().unwrap();
            assert!(
                input.iter().any(|i| i["type"] == "reasoning"
                    && i["encrypted_content"] == "opaque-provider-state")
            );
            assert!(
                input
                    .iter()
                    .any(|i| i["type"] == "function_call" && i["call_id"] == "c1")
            );
            assert!(
                input
                    .iter()
                    .any(|i| i["type"] == "function_call_output" && i["call_id"] == "c1")
            );
            assert!(
                body["input"]
                    .to_string()
                    .contains("original bulk rename rule")
            );
            Ok(message())
        })
        .unwrap();
    finish(&mut reopened, &mut host);
    assert!(
        host.undo.is_empty(),
        "historical calls must never be replayed"
    );
}
