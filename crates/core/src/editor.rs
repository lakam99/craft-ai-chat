//! Optional adapter helper for Craft's command/UI surfaces. The protocol also supports any custom tools.
use crate::{HostInfo, ToolCall, function};
use serde::Deserialize;
use serde_json::{Value, json};
pub(crate) mod discovery;

/// Opt in only when the adapter resolves discovered widget IDs to live screen geometry.
pub fn with_guidance(mut info: HostInfo) -> HostInfo {
    info.tools.push(function(
        "highlight_ui",
        "Outline discovered editor controls for the user without clicking them or editing the document. Inspect ui.elements first. Empty targets clears guidance.",
        json!({"type":"object","properties":{"targets":{"type":"array","maxItems":8,"items":{"type":"object","properties":{"id":{"type":"string","maxLength":256},"label":{"type":"string","maxLength":160}},"required":["id","label"],"additionalProperties":false}}},"required":["targets"],"additionalProperties":false}),
    ));
    info.instructions.push_str(" When the user asks how to do something, guide them instead of performing the edit. Inspect ui.elements, then use highlight_ui with actual visible widget IDs and short action labels. Match numbered instructions to the numbered outlines. Open a relevant panel or menu using advertised UI tools when necessary, inspect again, and highlight its controls. Do not execute the requested edit unless asked to do it. Never invent a target or claim a control was highlighted when the tool returned an error. Highlighting does not click controls, change values or change the document. Use an empty targets array to clear highlights.");
    info.tools.push(function("guide_state", "Read small, read-only editor state for verifying a walkthrough step. No credentials or UI drafts.", json!({"type":"object","properties":{},"additionalProperties":false})));
    info.tools.push(function("guide_ui", "Show ONE walkthrough step and wait for the user to complete it. This tool does not click or edit. It returns only after completion, confirmation, cancellation or a 30-minute timeout. Discover the visible target with ui.elements and state fields with guide_state first.", json!({
        "type":"object","properties":{
            "id":{"type":"string","maxLength":256,"description":"Visible widget ID to highlight."},
            "label":{"type":"string","maxLength":160,"description":"Short action label."},
            "instruction":{"type":"string","maxLength":4096,"description":"Instructions for this step only, in Markdown."},
            "completion":{"type":"string","enum":["activate","change","appear","disappear","state","confirm"],"description":"activate: button clicked; change: value edited and editing finished; appear/disappear: watched UI control changes visibility; state: editor result satisfies condition; confirm: user manually confirms."},
            "watch_id":{"type":"string","maxLength":256,"description":"Control watched by appear/disappear. Empty for other completion modes."},
            "state_pointer":{"type":"string","maxLength":256,"description":"JSON pointer from guide_state for state completion, otherwise empty."},
            "comparison":{"type":"string","enum":["equal","not_equal","greater"],"description":"State comparison; use equal for non-state completion."},
            "expected_json":{"type":"string","maxLength":1024,"description":"JSON scalar for state comparison, otherwise empty."}
        },"required":["id","label","instruction","completion","watch_id","state_pointer","comparison","expected_json"],"additionalProperties":false
    })));
    info.instructions.push_str(" For a step-by-step walkthrough, use guide_ui for just the CURRENT step. Explain it in instruction and wait for that tool to return before planning or highlighting the next step. After completion, inspect the updated editor with ui.elements/guide_state and adapt the next instruction. Do not perform the user's actions or open later steps for them. Prefer state completion to verify an actual result (such as an increased media count), not merely clicking Import or closing a dialog: cancelling or failing a dialog is not completion. Use activate for actions whose completion is the button activation itself, change for an observed value edit, appear/disappear for UI navigation only, and confirm for unobservable work. Never claim user_confirmation was automatically verified. Do not use highlight_ui to replace an interactive walkthrough. End with a concise summary when the goal is complete; respect cancellation. State fields and target IDs must be discovered, never guessed.");
    info
}

pub fn is_chat_target(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    id == "ai"
        || id.starts_with("ai.")
        || id.starts_with("settings.ai")
        || id.starts_with("preferences.ai")
        || id == "app.settings.ai"
        || id == "panel.aichat"
}

#[derive(Clone)]
pub struct EditorTools {
    pub name: &'static str,
    pub methods: &'static [&'static str],
    pub views: &'static [&'static str],
}
pub enum EditorCall {
    Command { method: String, params: Value },
    View(String),
}
impl EditorTools {
    pub fn definitions(&self) -> Vec<Value> {
        vec![
            function(
                self.name,
                "Discover and use existing commands and UI tools. engine.commands {} returns a topic index; use {filter: topic or exact ID, offset?: integer, limit?: 1..32} for full parameter schemas. Reuse schemas already discovered instead of repeating catalogs. Run commands with engine.execute {command,params}.",
                json!({"type":"object","properties":{"method":{"type":"string","enum":self.methods},"params_json":{"type":"string","description":"JSON object; {} when none needed. For engine.commands use filter (not query/search), then optional offset/limit to page results."}},"required":["method","params_json"],"additionalProperties":false}),
            ),
            function(
                "video_frame",
                "Capture an active view as a still image without seeking or capturing the desktop or chat.",
                json!({"type":"object","properties":{"monitor":{"type":"string","enum":self.views}},"required":["monitor"],"additionalProperties":false}),
            ),
        ]
    }
    pub fn automatic_inputs(&self) -> Vec<ToolCall> {
        self.views
            .iter()
            .map(|view| ToolCall::new("video_frame", json!({"monitor":view})))
            .collect()
    }
    pub fn parse(&self, name: &str, args: &Value) -> Result<EditorCall, String> {
        if name == self.name {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Args {
                method: String,
                params_json: String,
            }
            let args: Args = serde_json::from_value(args.clone())
                .map_err(|_| "Expected method and params_json strings")?;
            if !self.methods.contains(&args.method.as_str()) {
                return Err("Unsupported host method; use the advertised tools".into());
            }
            if args.params_json.len() > 64 * 1024 {
                return Err("Editor parameters exceed 64 KiB".into());
            }
            let mut params: Value = serde_json::from_str(&args.params_json)
                .map_err(|_| "params_json must contain a valid JSON object")?;
            if !params.is_object() {
                return Err("params_json must contain a JSON object".into());
            }
            discovery::prepare(&args.method, &mut params)?;
            if targets_chat(&params, 0) {
                return Err("AI chat controls and preferences are reserved for the user".into());
            }
            Ok(EditorCall::Command {
                method: args.method,
                params,
            })
        } else if name == "video_frame" {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Args {
                monitor: String,
            }
            let args: Args =
                serde_json::from_value(args.clone()).map_err(|_| "Expected an active view name")?;
            if !self.views.contains(&args.monitor.as_str()) {
                return Err("This active view is not advertised by the host".into());
            }
            Ok(EditorCall::View(args.monitor))
        } else {
            Err("Unknown AI tool".into())
        }
    }
    pub fn host_info(
        &self,
        id: &str,
        name: &str,
        instructions: &str,
        attachment_label: &str,
    ) -> HostInfo {
        let info = HostInfo {
            id: id.into(),
            name: name.into(),
            instructions: instructions.into(),
            tools: self.definitions(),
            automatic_inputs: self.automatic_inputs(),
            attachment_label: attachment_label.into(),
            welcome: format!("Ask about your work, request edits, or use {name}'s tools."),
        };
        if self.methods.contains(&"engine.execute") {
            crate::batch::with_batches(info, self.name)
        } else {
            info
        }
    }
}

pub(crate) fn targets_chat(value: &Value, depth: usize) -> bool {
    if depth > 32 {
        return true;
    }
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            key == "ai"
                || key.starts_with("ai.")
                || matches!(key.as_str(), "command" | "id" | "key" | "page")
                    && value.as_str().is_some_and(|id| {
                        id == "ai"
                            || id.starts_with("ai.")
                            || id.starts_with("settings.ai.")
                            || id == "app.settings.ai"
                    })
                || targets_chat(value, depth + 1)
        }),
        Value::Array(values) => values.iter().any(|v| targets_chat(v, depth + 1)),
        _ => false,
    }
}
