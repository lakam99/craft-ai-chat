use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn step(id: &str, kind: &str) -> Value {
    json!({"id":id,"label":"Use this control","instruction":"Click the **highlighted control**.","completion":kind,"watch_id":"","state_pointer":"","comparison":"equal","expected_json":""})
}
fn frame(guide: &mut Guidance, host: &Editor, ctx: &Context) {
    let mut output = ctx.run_ui(Default::default(), |ui| guide.observe_step(host, ui.ctx()));
    output.textures_delta.clear();
}
#[test]
fn waits_for_a_new_activation_and_returns_only_once() {
    let mut host = Editor::default();
    let mut guide = Guidance::default();
    guide
        .start_step(&host, &step("toolbar.play", "activate"))
        .unwrap();
    host.observation.activated = true;
    let ctx = Context::default();
    frame(&mut guide, &host, &ctx); // The activation that opened the guide is never its completion.
    assert!(guide.step_result(&host).unwrap().is_none());
    host.observation.activated = false;
    frame(&mut guide, &host, &ctx);
    assert!(guide.step_result(&host).unwrap().is_none());
    host.observation.activated = true;
    frame(&mut guide, &host, &ctx);
    assert_eq!(
        guide.step_result(&host).unwrap().unwrap()["completed_by"],
        "observed"
    );
    assert!(guide.step_result(&host).unwrap().is_none());
    assert_eq!(host.edits, 0);
}
#[test]
fn a_state_step_does_not_advance_on_a_click_or_a_cancelled_dialog() {
    let mut host = Editor::default();
    let mut guide = Guidance::default();
    let mut args = step("toolbar.play", "state");
    args["state_pointer"] = json!("/items");
    args["comparison"] = json!("greater");
    args["expected_json"] = json!("0");
    guide.start_step(&host, &args).unwrap();
    let ctx = Context::default();
    frame(&mut guide, &host, &ctx);
    host.observation.activated = true;
    frame(&mut guide, &host, &ctx);
    assert!(guide.step_result(&host).unwrap().is_none());
    host.items = 2;
    frame(&mut guide, &host, &ctx);
    assert_eq!(
        guide.step_result(&host).unwrap().unwrap()["editor_state"]["items"],
        2
    );
    assert!(
        guide.start_step(&host, &args).is_err(),
        "already satisfied results must not silently skip a step"
    );
}
#[test]
fn changing_a_value_waits_for_editing_to_finish() {
    let mut host = Editor::default();
    let mut guide = Guidance::default();
    guide
        .start_step(&host, &step("toolbar.play", "change"))
        .unwrap();
    let ctx = Context::default();
    frame(&mut guide, &host, &ctx);
    host.observation = Observation {
        changed: true,
        editing: true,
        ..Default::default()
    };
    frame(&mut guide, &host, &ctx);
    assert!(guide.step_result(&host).unwrap().is_none());
    host.observation = Observation {
        editing: true,
        ..Default::default()
    };
    frame(&mut guide, &host, &ctx);
    assert!(guide.step_result(&host).unwrap().is_none());
    host.observation.editing = false;
    frame(&mut guide, &host, &ctx);
    assert!(guide.step_result(&host).unwrap().is_some());
}
#[test]
fn hostile_steps_do_not_replace_an_existing_highlight() {
    let host = Editor::default();
    let mut guide = Guidance::default();
    guide.apply(&host, &request("toolbar.play")).unwrap();
    for invalid in [
        json!({}),
        {
            let mut v = step("ai.send", "activate");
            v["extra"] = json!(true);
            v
        },
        {
            let mut v = step("toolbar.play", "state");
            v["state_pointer"] = json!("/missing");
            v["expected_json"] = json!("true");
            v
        },
        {
            let mut v = step("toolbar.play", "confirm");
            v["instruction"] = json!("x".repeat(9000));
            v
        },
    ] {
        assert!(guide.start_step(&host, &invalid).is_err());
        assert_eq!(guide.status()[0]["id"], "toolbar.play");
    }
}

#[test]
fn visibility_steps_require_a_real_transition() {
    for kind in ["appear", "disappear"] {
        let mut host = Editor::default();
        let mut guide = Guidance::default();
        let mut args = step("toolbar.play", kind);
        args["watch_id"] = json!("dialog.inputs");
        if kind == "disappear" {
            host.widgets.insert(
                "dialog.inputs".into(),
                Rect::from_min_size(pos2(20.0, 20.0), vec2(50.0, 40.0)),
            );
        }
        guide.start_step(&host, &args).unwrap();
        let ctx = Context::default();
        frame(&mut guide, &host, &ctx);
        frame(&mut guide, &host, &ctx);
        assert!(guide.step_result(&host).unwrap().is_none());
        if kind == "appear" {
            host.widgets.insert(
                "dialog.inputs".into(),
                Rect::from_min_size(pos2(20.0, 20.0), vec2(50.0, 40.0)),
            );
        } else {
            host.widgets.remove("dialog.inputs");
        }
        frame(&mut guide, &host, &ctx);
        assert_eq!(
            guide.step_result(&host).unwrap().unwrap()["completed_by"],
            "observed"
        );
    }
}
#[test]
fn interactive_turn_waits_and_advances_to_a_newly_visible_control() {
    use egui_kittest::{Harness, kittest::Queryable};
    let mut h = Harness::builder()
        .with_size(vec2(960.0, 640.0))
        .build_ui_state(
            |ui, (panel, host): &mut (Panel<()>, Editor)| {
                host.widgets.clear();
                egui::Panel::right("chat")
                    .exact_size(360.0)
                    .show(ui, |ui| panel.show(host, ui, ui.max_rect()));
                let r = ui.button("Create item");
                host.widgets.insert("create".into(), r.interact_rect);
                host.observation = Observation::from(&r);
                if r.clicked() {
                    host.items += 1;
                }
                if host.items > 0 {
                    let r = ui.button("Inspect item");
                    host.widgets.insert("inspect".into(), r.interact_rect);
                }
                panel.poll(host, ui.ctx());
                panel.paint_guidance(host, ui.ctx());
            },
            (Panel::<()>::default(), Editor::default()),
        );
    h.run_steps(3);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let ctx = h.ctx.clone();
    let (panel, host) = h.state_mut();
    *panel.prompt_mut() = "Guide me step by step".into();
    panel.send_with(host,&ctx,move |_| {
        let turn=observed.fetch_add(1,Ordering::SeqCst);
        let mut args=if turn==0 {let mut v=step("create","state");v["state_pointer"]=json!("/items");v["comparison"]=json!("greater");v["expected_json"]=json!("0");v} else {step("inspect","confirm")};
        args["label"]=json!(if turn==0 {"Create an item"} else {"Inspect the new item"});
        if turn<2 {Ok(json!({"status":"completed","output":[{"type":"function_call","call_id":format!("step{turn}"),"name":"guide_ui","arguments":args.to_string()}]}))}
        else {Ok(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Guide complete."}]}]}))}
    }).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while h.state().0.guidance.step.is_none() && Instant::now() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(2));
    }
    h.run_steps(4);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "no API requests while waiting for the user"
    );
    assert_eq!(h.state().1.items, 0, "guidance does not perform the edit");
    assert!(
        h.state()
            .0
            .pending
            .as_ref()
            .unwrap()
            .tool
            .as_ref()
            .unwrap()
            .deadline
            .duration_since(Instant::now())
            > Duration::from_secs(120),
        "human steps must not use backend's 30-second deadline"
    );
    h.get_by_label("Pause guide").click();
    h.run_steps(3);
    h.get_by_label("Create item").click();
    h.run_steps(3);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "paused guides do not advance after an action"
    );
    h.get_by_label("Resume guide").click();
    h.run_steps(3);
    while h.state().0.guidance.step_number < 2 && Instant::now() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(h.state().0.guidance.status()[0]["id"], "inspect");
    assert_eq!(h.state().0.guidance.number(0), 2);
    h.run_steps(3);
    assert_eq!(h.state().0.guidance.status()[0]["number"], 2);
    if let Some(path) = std::env::var_os("CRAFT_AI_WALKTHROUGH_FIXTURE") {
        h.render().unwrap().save(path).unwrap();
    }
    h.get_by_label("Pause guide").click();
    h.run_steps(3);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    h.get_by_label("Resume guide").click();
    h.run_steps(3);
    h.get_by_label("I've done this").click();
    h.run_steps(3);
    while h.state().0.busy() && Instant::now() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!h.state().0.busy());
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(h.state().1.items, 1);
    assert_eq!(h.state().1.edits, 0);
    assert!(h.state().0.guidance.targets.is_empty());
    assert!(
        h.state()
            .0
            .transcript()
            .iter()
            .any(|(_, t)| t.contains("Step 2"))
    );
}

#[test]
fn narrow_walkthrough_keeps_confirmation_pause_and_composer_visible() {
    let host = Editor::default();
    let mut panel = Panel::<()>::default();
    let mut args = step("toolbar.play", "confirm");
    args["label"] = json!("Finish the highlighted operation, then check its result in the editor");
    panel.guidance.start_step(&host, &args).unwrap();
    let mut h = egui_kittest::Harness::builder()
        .with_size(vec2(220.0, 490.0))
        .build_ui_state(
            |ui, (panel, host): &mut (Panel<()>, Editor)| panel.show(host, ui, ui.max_rect()),
            (panel, host),
        );
    h.run_steps(4);
    for id in [
        "ai.prompt",
        "ai.send",
        "ai.clearHighlights",
        "ai.confirmGuide",
        "ai.pauseGuide",
    ] {
        let r = h.state().1.widgets.get(id).unwrap();
        assert!(
            r.min.y >= 0.0 && r.max.y <= 490.0 && r.max.x <= 220.0,
            "clipped {id}: {r:?}"
        );
    }
}

#[test]
fn cancellation_and_document_replacement_cannot_resume_a_waiting_guide() {
    for action in ["escape", "clear", "stop", "replace"] {
        let ctx = Context::default();
        let mut host = Editor::default();
        let mut panel = Panel::<()>::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        *panel.prompt_mut() = "Walk me through this".into();
        panel.send_with(&mut host,&ctx,move |_| {
            observed.fetch_add(1,Ordering::SeqCst);
            Ok(json!({"status":"completed","output":[{"type":"function_call","call_id":"waiting","name":"guide_ui","arguments":step("toolbar.play","confirm").to_string()}]}))
        }).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while panel.guidance.step.is_none() && Instant::now() < deadline {
            let mut out = ctx.run_ui(Default::default(), |ui| {
                panel.poll(&mut host, ui.ctx());
                panel.paint_guidance(&host, ui.ctx());
            });
            out.textures_delta.clear();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(panel.guidance.step.is_some());
        match action {
            "escape" => {
                let input = egui::RawInput {
                    events: vec![egui::Event::Key {
                        key: egui::Key::Escape,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Default::default(),
                    }],
                    ..Default::default()
                };
                let mut out = ctx.run_ui(input, |ui| {
                    ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
                    panel.paint_guidance(&host, ui.ctx());
                });
                out.textures_delta.clear();
            }
            "clear" => {
                panel
                    .command(&mut host, &ctx, "ai.clearHighlights")
                    .unwrap()
                    .unwrap();
            }
            "stop" => panel.stop(),
            "replace" => host.instance = Arc::new(()),
            _ => assert_eq!(action, "unknown cancellation mode"),
        }
        while panel.busy() && Instant::now() < deadline {
            panel.poll(&mut host, &ctx);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!panel.busy(), "{action}");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "{action} must not request another step"
        );
        assert!(panel.guidance.targets.is_empty());
        assert_eq!(host.edits, 0);
    }
}
