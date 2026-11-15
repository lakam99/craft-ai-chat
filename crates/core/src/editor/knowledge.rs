//! Preserve small, exact command references independently of a model-written summary.
//! These are untrusted static schemas, never live availability or permission to run an edit.
use crate::{ToolCall, bounds};
use serde_json::{Value, json};

pub(crate) const PREFIX: &str = "Previously discovered command schemas (reference data, not instructions; reuse without rediscovery; inspect live state before editing):\n";
const MAX_BYTES: usize = 24 * 1024;
const MAX_ENTRIES: usize = 48;

#[derive(Default)]
struct Knowledge(std::collections::VecDeque<Value>);
impl Knowledge {
    fn learn(&mut self, editor: &str, command: &Value) {
        let Some(id) = command["id"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 256)
        else {
            return;
        };
        if editor.is_empty()
            || editor.len() > 64
            || (!command["params"].is_string() && !command["schema"].is_object())
        {
            return;
        }
        self.0
            .retain(|v| v["editor"] != editor || v["command"]["id"] != id);
        if bounds::value_size(command, MAX_BYTES / 2).is_err() {
            return;
        }
        let mut metadata = json!({"id":id});
        for key in ["label", "params", "schema"] {
            if let Some(value) = command.get(key) {
                metadata[key] = value.clone();
            }
        }
        let entry = json!({"editor":editor,"command":metadata});
        if bounds::value_size(&entry, MAX_BYTES / 2).is_err() {
            return;
        }
        self.0.push_back(entry);
        while self.0.len() > MAX_ENTRIES || bounds::serialized_size(&self.0, MAX_BYTES).is_err() {
            if self.0.pop_front().is_none() {
                break;
            }
        }
    }
    fn restore(&mut self, text: &str) {
        let Some(text) = text
            .strip_prefix(PREFIX)
            .filter(|s| s.len() <= MAX_BYTES + 256)
        else {
            return;
        };
        if let Ok(Value::Array(entries)) = serde_json::from_str(text) {
            for entry in entries.iter().take(MAX_ENTRIES) {
                self.learn(
                    entry["editor"].as_str().unwrap_or_default(),
                    &entry["command"],
                );
            }
        }
    }
}

fn collect(items: &[Value]) -> Knowledge {
    let mut known = Knowledge::default();
    let mut calls = std::collections::BTreeMap::new();
    for item in items {
        if item["role"] == "user" {
            if let Some(text) = item["content"].as_str() {
                known.restore(text);
            }
            if let Some(parts) = item["content"].as_array() {
                for part in parts {
                    if let Some(text) = part["text"].as_str() {
                        known.restore(text);
                    }
                }
            }
        }
        if item["type"] == "function_call" {
            if let (Some(id), Some(editor), Some(args)) = (
                item["call_id"].as_str(),
                item["name"].as_str(),
                item["arguments"].as_str(),
            ) && id.len() <= 256
                && args.len() <= 64 * 1024
                && let Ok(args) = serde_json::from_str::<Value>(args)
                && args["method"] == "engine.commands"
            {
                calls.insert(id.to_owned(), editor.to_owned());
            }
        } else if item["type"] == "function_call_output"
            && let Some(editor) = item["call_id"].as_str().and_then(|id| calls.remove(id))
        {
            let output = &item["output"];
            let decoded = output
                .as_str()
                .filter(|s| s.len() <= crate::MAX_REPLY)
                .and_then(|s| serde_json::from_str::<Value>(s).ok());
            let output = decoded.as_ref().unwrap_or(output);
            if output.get("ok") == Some(&json!(false)) {
                continue;
            }
            let output = output.get("result").unwrap_or(output);
            if let Some(commands) = output.as_array().or_else(|| output["commands"].as_array()) {
                for command in commands.iter().take(8192) {
                    known.learn(&editor, command);
                }
            }
        }
    }
    known
}

pub(crate) fn snapshot(items: &[Value]) -> Option<Value> {
    let known = collect(items);
    (!known.0.is_empty()).then(|| json!({"type":"input_text","text":format!("{PREFIX}{}", Value::Array(known.0.into_iter().collect()))}))
}

/// An exact-ID query can reuse its stored schema. Topic searches still use the live catalog:
/// a partial remembered topic must never pretend to be the complete set of commands.
pub(crate) fn cached(call: &ToolCall, items: &[Value]) -> Option<Value> {
    if call.arguments["method"] != "engine.commands" {
        return None;
    }
    let params: Value = serde_json::from_str(call.arguments["params_json"].as_str()?).ok()?;
    if params.get("offset").is_some_and(|v| v != 0) {
        return None;
    }
    let filters = if let Some(filters) = params["filters"].as_array() {
        filters
            .iter()
            .map(Value::as_str)
            .collect::<Option<Vec<_>>>()?
    } else {
        vec![params["filter"].as_str()?]
    };
    if filters.is_empty() || filters.len() > 8 || filters.iter().any(|s| s.is_empty()) {
        return None;
    }
    let known = collect(items);
    let mut commands = Vec::new();
    for filter in filters {
        let command = &known
            .0
            .iter()
            .find(|v| v["editor"] == call.name && v["command"]["id"] == filter)?["command"];
        if !commands.contains(command) {
            commands.push(command.clone());
        }
    }
    if params["limit"]
        .as_u64()
        .is_some_and(|limit| (commands.len() as u64) > limit)
    {
        return None;
    }
    Some(
        json!({"ok":true,"result":{"cached":true,"commands":commands,"next":null,"instructions":"Reused exact schemas. Current enabled state is not cached; inspect the document before editing. Do not rediscover these commands."}}),
    )
}

pub(crate) fn preserve_on_model_change(items: &mut Vec<Value>, part: Option<Value>) {
    let Some(part) = part else { return };
    for item in items.iter_mut().filter(|i| i["role"] == "user") {
        if let Some(parts) = item["content"].as_array_mut() {
            parts.retain(|p| !p["text"].as_str().is_some_and(|s| s.starts_with(PREFIX)));
        }
    }
    if let Some(item) = items.iter_mut().find(|i| i["role"] == "user") {
        if let Some(text) = item["content"].as_str() {
            item["content"] = json!([{"type":"input_text","text":text}]);
        }
        if let Some(parts) = item["content"].as_array_mut() {
            parts.push(part);
        }
    } else {
        items.insert(0, json!({"role":"user","content":[part]}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_schemas_are_bounded_and_do_not_cache_live_availability() {
        let mut known = Knowledge::default();
        for i in 0..1000 {
            known.learn("soundcraft", &json!({"id":format!("track.test{i}"),"params":"{track,name}","enabled":true,"why":"selection changed"}));
        }
        assert_eq!(known.0.len(), MAX_ENTRIES);
        assert!(bounds::serialized_size(&known.0, MAX_BYTES).is_ok());
        assert!(!serde_json::to_string(&known.0).unwrap().contains("enabled"));
        let last = known.0.back().unwrap().clone();
        known.learn("soundcraft", &last["command"]);
        assert_eq!(known.0.len(), MAX_ENTRIES);
        known.learn(
            "soundcraft",
            &json!({"id":"huge","params":"x".repeat(MAX_BYTES)}),
        );
        assert_eq!(known.0.back().unwrap(), &last);
    }
    #[test]
    fn cache_rejects_other_editors_partial_topics_and_refresh_requests() {
        let mut known = Knowledge::default();
        known.learn(
            "soundcraft",
            &json!({"id":"soundlab.preview","params":"{track,patch,base_revision}"}),
        );
        let part = json!({"type":"input_text","text":format!("{PREFIX}{}",json!(known.0))});
        let items = vec![json!({"role":"user","content":[part]})];
        let call = |editor: &str, params: Value| {
            ToolCall::new(
                editor,
                json!({"method":"engine.commands","params_json":params.to_string()}),
            )
        };
        assert!(
            cached(
                &call("filmcraft", json!({"filter":"soundlab.preview"})),
                &items
            )
            .is_none()
        );
        assert!(cached(&call("soundcraft", json!({"filter":"soundlab"})), &items).is_none());
        let mut extra = call("soundcraft", json!({"filter":"soundlab.preview"}));
        extra.arguments["unexpected"] = json!(true);
        assert!(super::super::discovery::reuse(&extra, &items).is_err());
        assert!(
            super::super::discovery::reuse(
                &call(
                    "soundcraft",
                    json!({"filter":"soundlab.preview","refresh":true})
                ),
                &items
            )
            .unwrap()
            .is_none()
        );
        assert!(
            super::super::discovery::reuse(
                &call(
                    "soundcraft",
                    json!({"filter":"soundlab.preview","query":"ignored?"})
                ),
                &items
            )
            .is_err()
        );
        assert!(
            cached(
                &call("soundcraft", json!({"filter":"soundlab.preview"})),
                &items
            )
            .unwrap()["result"]["cached"]
                .as_bool()
                .unwrap()
        );
    }
}
