use craft_ai_chat_core::{HostInfo, ToolCall, bounds, editor::EditorTools};
use serde_json::{Value, json};
fn info() -> HostInfo {
    EditorTools {
        name: "example",
        methods: &["engine.execute"],
        views: &["Canvas"],
    }
    .host_info("example", "Example Craft", "", "Attach active canvas")
}
#[test]
fn only_advertised_tools_and_views_can_be_dispatched() {
    let host = info();
    host.validate().unwrap();
    assert!(
        host.validate_call(&ToolCall::new("shell", json!({})))
            .is_err()
    );
    let tools = EditorTools {
        name: "example",
        methods: &["engine.execute", "ui.focus"],
        views: &["Canvas"],
    };
    assert!(
        tools
            .parse("video_frame", &json!({"monitor":"Desktop"}))
            .is_err()
    );
    for params in [
        json!({"command":"ai.send"}),
        json!({"command":"prefs.set","params":{"key":"ai.model","value":"wrong"}}),
        json!({"id":"settings.ai.key"}),
    ] {
        assert!(
            tools
                .parse(
                    "example",
                    &json!({"method":"engine.execute","params_json":params.to_string()})
                )
                .is_err()
        );
    }
}
#[test]
fn duplicate_schemas_and_invalid_automatic_inputs_are_rejected() {
    let mut host = info();
    host.tools.push(host.tools[0].clone());
    assert!(host.validate().is_err());
    let mut host = info();
    host.automatic_inputs
        .push(ToolCall::new("missing", json!({})));
    assert!(host.validate().is_err());
}
#[test]
fn json_size_and_depth_are_bounded_without_a_large_second_buffer() {
    assert!(bounds::value_size(&json!({"value":"a".repeat(1024)}), 100).is_err());
    let mut deep = Value::Null;
    for _ in 0..80 {
        deep = json!([deep]);
    }
    assert!(bounds::value_size(&deep, 4096).is_err());
    assert_eq!(bounds::value_size(&json!({"a":1}), 100).unwrap(), 7);
}
