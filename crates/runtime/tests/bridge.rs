//! Real process/pipe checks. Never contacts OpenAI or reads a real preference profile.
use craft_ai_chat_egui::core::{
    Preferences,
    editor::EditorTools,
    runtime::{self, Input, Output, Snapshot},
};
use serde_json::json;
use std::{
    io::BufReader,
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

#[test]
fn standalone_binary_retains_license_notices() {
    let output = Command::new(env!("CARGO_BIN_EXE_craft-ai-chat-runtime"))
        .arg("--licenses")
        .output()
        .unwrap();
    assert!(output.status.success());
    let notices = String::from_utf8(output.stdout).unwrap();
    for required in [
        "Craft AI Chat",
        "Apache License",
        "Permission is hereby granted",
        "pulldown-cmark",
    ] {
        assert!(notices.contains(required), "{required}");
    }
}

#[test]
fn standalone_component_handshakes_renders_and_keeps_secrets_out_of_views() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_craft-ai-chat-runtime"))
        .arg("--stdio-v1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Ok(Some(packet)) = runtime::read::<Output>(&mut reader) {
            if send.send(packet).is_err() {
                break;
            }
        }
    });
    let ready = receive.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(matches!(
        ready,
        Output::Ready {
            protocol: runtime::PROTOCOL,
            ..
        }
    ));
    let snapshot = Snapshot {
        protocol: runtime::PROTOCOL,
        epoch: 7,
        info: EditorTools {
            name: "test",
            methods: &["engine.commands", "engine.execute"],
            views: &[],
        }
        .host_info("test", "Test editor", "Test only", "No image"),
        preferences: Preferences {
            remember_key: false,
            remember_history: false,
            ..Preferences::default()
        },
        document: None,
        profile: None,
        open: true,
    };
    runtime::write(
        &mut input,
        &Input::Sync {
            snapshot: snapshot.clone(),
        },
    )
    .unwrap();
    let view = receive.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(matches!(view, Output::View { epoch: 7, .. }));
    let secret = "LOCAL-TEST-KEY-NEVER-USE";
    runtime::write(
        &mut input,
        &Input::Action {
            serial: 1,
            snapshot: snapshot.clone(),
            id: "ai.key".into(),
            value: json!(secret),
        },
    )
    .unwrap();
    let view = receive.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(!serde_json::to_string(&view).unwrap().contains(secret));
    runtime::write(
        &mut input,
        &Input::Action {
            serial: 2,
            snapshot: snapshot.clone(),
            id: "ai.prompt".into(),
            value: json!("A new draft"),
        },
    )
    .unwrap();
    loop {
        let view = receive.recv_timeout(Duration::from_secs(10)).unwrap();
        let encoded = serde_json::to_string(&view).unwrap();
        assert!(!encoded.contains(secret));
        if encoded.contains("A new draft") {
            break;
        }
    }
    runtime::write(
        &mut input,
        &Input::Action {
            serial: 3,
            snapshot: snapshot.clone(),
            id: "ai.key".into(),
            value: json!(""),
        },
    )
    .unwrap();
    runtime::write(
        &mut input,
        &Input::Action {
            serial: 4,
            snapshot,
            id: "ai.send".into(),
            value: json!("Retain my unsent instruction"),
        },
    )
    .unwrap();
    loop {
        let view = receive.recv_timeout(Duration::from_secs(10)).unwrap();
        let encoded = serde_json::to_string(&view).unwrap();
        if matches!(
            view,
            Output::View {
                submitted: Some(runtime::Receipt {
                    serial: 4,
                    accepted: false
                }),
                ..
            }
        ) {
            assert!(encoded.contains("Retain my unsent instruction"));
            break;
        }
    }
    runtime::write(&mut input, &Input::Shutdown).unwrap();
    drop(input);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn compatibility_probe_is_bounded_and_does_not_start_an_ai_session() {
    let output = Command::new(env!("CARGO_BIN_EXE_craft-ai-chat-runtime"))
        .arg("--probe")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(matches!(
        serde_json::from_slice::<Output>(&output.stdout).unwrap(),
        Output::Ready {
            protocol: runtime::PROTOCOL,
            ..
        }
    ));
    assert!(output.stderr.is_empty());
}

#[test]
fn incompatible_protocol_exits_before_loading_a_profile() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_craft-ai-chat-runtime"))
        .arg("--stdio-v1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let snapshot = Snapshot {
        protocol: runtime::PROTOCOL + 1,
        epoch: 1,
        info: EditorTools {
            name: "test",
            methods: &["engine.commands"],
            views: &[],
        }
        .host_info("test", "Test", "test", "none"),
        preferences: Preferences::default(),
        document: None,
        profile: None,
        open: true,
    };
    runtime::write(child.stdin.as_mut().unwrap(), &Input::Sync { snapshot }).unwrap();
    drop(child.stdin.take());
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(!status.success());
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    use std::io::Read;
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(stderr.is_empty());
}
