use super::*;
use crate::{DEFAULT_MODEL, editor::EditorTools};
fn host() -> crate::HostInfo {
    EditorTools {
        name: "effectcraft",
        methods: &["engine.commands", "engine.execute"],
        views: &["Composition"],
    }
    .host_info("example", "Example", "", "Attach active views")
}

fn message(text: &str) -> Value {
    json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}]})
}
fn function(args: &str) -> Value {
    json!({"status":"completed","output":[
        {"type":"reasoning","id":"r1","summary":[],"encrypted_content":"opaque"},
        {"type":"function_call","id":"f1","call_id":"c1","name":"effectcraft","arguments":args}
    ]})
}

#[test]
fn a_large_command_catalog_does_not_force_summary_before_the_next_editor_step() {
    let catalog: Vec<_> = (0..600)
        .map(|i| json!({"id":format!("synth.control{i}"),"label":format!("Synth control {i}"),"params":format!("{{track,value}} {}","details ".repeat(30)),"enabled":true}))
        .collect();
    assert!(serde_json::to_vec(&catalog).unwrap().len() > crate::history::COMPACT_BYTES);
    let mut turns = 0;
    let result = run(
        &host(), "gpt-6-luna", vec![json!({"role":"user","content":"Make my MIDI track sound like a trumpet; keep its notes and name"})],
        &AtomicBool::new(false),
        |body| {
            assert!(!body["tools"].as_array().unwrap().is_empty(), "A read-only command catalog must not cause an extra summarization request");
            turns += 1;
            if turns == 1 { return Ok(function(r#"{"method":"engine.commands","params_json":"{}"}"#)); }
            let output: Value = serde_json::from_str(body["input"].as_array().unwrap().last().unwrap()["output"].as_str().unwrap()).unwrap();
            assert!(output["result"]["topics"].is_array());
            assert!(body["input"].to_string().contains("keep its notes and name"));
            Ok(message("I can discover the synth controls without changing your notes."))
        },
        |_, _| Ok(json!({"ok":true,"result":catalog})),
    ).unwrap();
    assert_eq!(turns, 2);
    assert!(result.text.contains("synth controls"));
}

#[test]
fn responses_round_trip_preserves_reasoning_call_ids_and_image_results() {
    let mut requests = Vec::new();
    let mut calls = 0;
    let frame = json!([{"type":"input_text","text":"Composition frame 0"},{"type":"input_image","image_url":"data:image/png;base64,test","detail":"auto"}]);
    let done = run(
        &host(),
        DEFAULT_MODEL,
        vec![json!({"role":"user","content":"inspect shot"})],
        &AtomicBool::new(false),
        |body| {
            requests.push(body.clone());
            if requests.len() == 1 {
                Ok(function(
                    r#"{"method":"engine.commands","params_json":"{}"}"#,
                ))
            } else {
                Ok(message("I inspected the shot."))
            }
        },
        |name, args| {
            calls += 1;
            assert_eq!(name, "effectcraft");
            assert_eq!(args["method"], "engine.commands");
            Ok(frame.clone())
        },
    )
    .unwrap();
    assert_eq!(calls, 1);
    assert_eq!(done.text, "I inspected the shot.");
    assert_eq!(requests[0]["store"], false);
    assert_eq!(requests[0]["parallel_tool_calls"], false);
    assert_eq!(requests[1]["input"][1]["encrypted_content"], "opaque");
    assert_eq!(requests[1]["input"][3]["call_id"], "c1");
    assert_eq!(requests[1]["input"][3]["output"], frame);
    let second = run(
        &host(),
        DEFAULT_MODEL,
        done.history,
        &AtomicBool::new(false),
        |body| {
            assert!(
                body["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|i| i["type"] == "function_call_output")
            );
            Ok(message("Still here."))
        },
        |_, _| panic!("No tool call expected"),
    )
    .unwrap();
    assert_eq!(second.text, "Still here.");
}

#[test]
fn repeated_full_discovery_is_redirected_without_refetching_the_catalog() {
    let mut turn = 0;
    let mut dispatches = 0;
    run(
        &host(),
        DEFAULT_MODEL,
        vec![],
        &AtomicBool::new(false),
        |body| {
            turn += 1;
            match turn {
                1 | 2 => Ok(function(
                    r#"{"method":"engine.commands","params_json":"{}"}"#,
                )),
                3 => {
                    assert!(
                        body["input"].as_array().unwrap().last().unwrap()["output"]
                            .as_str()
                            .unwrap()
                            .contains("already discovered")
                    );
                    Ok(function(
                        r#"{"method":"engine.commands","params_json":"{\"filter\":\"synth\"}"}"#,
                    ))
                }
                _ => Ok(message("Use the filtered synth schemas.")),
            }
        },
        |_, _| {
            dispatches += 1;
            Ok(json!({"ok":true,"result":[{"id":"synth.catalog","params":"{}"}]}))
        },
    )
    .unwrap();
    assert_eq!(dispatches, 2);
    assert_eq!(turn, 4);
}

#[test]
fn malformed_arguments_return_tool_error_without_executing() {
    let mut n = 0;
    run(
        &host(),
        DEFAULT_MODEL,
        vec![],
        &AtomicBool::new(false),
        |body| {
            n += 1;
            if n == 1 {
                Ok(function("{bad"))
            } else {
                assert!(
                    body["input"].as_array().unwrap().last().unwrap()["output"]
                        .as_str()
                        .unwrap()
                        .contains("JSON object")
                );
                Ok(message("The call was invalid."))
            }
        },
        |_, _| panic!("Malformed arguments must not execute"),
    )
    .unwrap();
}

#[test]
fn cancellation_between_api_and_tools_prevents_mutation() {
    let cancel = AtomicBool::new(false);
    let result = run(
        &host(),
        DEFAULT_MODEL,
        vec![],
        &cancel,
        |_| {
            cancel.store(true, Ordering::Release);
            Ok(function(
                r#"{"method":"engine.execute","params_json":"{}"}"#,
            ))
        },
        |_, _| panic!("Cancelled tool must not execute"),
    );
    assert!(result.err().unwrap().contains("Stopped"));
}

#[test]
fn runaway_tool_loop_is_bounded() {
    let mut calls = 0;
    let result = run(
        &host(),
        DEFAULT_MODEL,
        vec![],
        &AtomicBool::new(false),
        |_| Ok(function("{}")),
        |_, _| {
            calls += 1;
            Ok(Value::String("done".into()))
        },
    );
    assert_eq!(calls, MAX_CALLS);
    assert!(result.err().unwrap().contains("limit"));
}

#[test]
fn incomplete_or_hostile_responses_never_dispatch_tools() {
    for response in [
        Value::Null,
        json!({"status":"failed","error":{"message":"secret"}}),
        json!({"status":"completed","output":{}}),
    ] {
        assert!(
            run(
                &host(),
                DEFAULT_MODEL,
                vec![],
                &AtomicBool::new(false),
                |_| Ok(response.clone()),
                |_, _| panic!("Invalid response")
            )
            .is_err()
        );
    }
    for key in ["", "sk-bad\ncredential", &"a".repeat(513)] {
        assert!(crate::validate_key(key).is_err());
    }
}
