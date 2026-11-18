//! Detect unchanged observations, not repeated edits: toggles, nudges and job polling are valid.
use crate::ToolCall;
use serde_json::Value;
use std::{
    collections::{BTreeMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
};

#[derive(Default)]
pub(super) struct Progress {
    observations: BTreeMap<String, (u64, usize)>,
    recover: bool,
    finish: bool,
}
impl Progress {
    pub fn recover(&self) -> bool {
        self.recover
    }
    pub fn finish(&self) -> bool {
        self.finish
    }
    pub fn observe(&mut self, call: &ToolCall, result: &Value) -> Result<(), String> {
        let result = result
            .as_str()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .unwrap_or_else(|| result.clone());
        let failed = result["ok"] == false;
        let mut args = call.arguments.clone();
        if let Some(text) = args["params_json"].as_str()
            && let Ok(params) = serde_json::from_str::<Value>(text)
        {
            args["params_json"] = params;
        }
        let method = args["method"].as_str().unwrap_or(&call.name);
        let command = args["params_json"]["command"].as_str().unwrap_or(method);
        // Long-running jobs can legitimately report unchanged status. Their asynchronous
        // completion/cancellation is owned by the adapter, never this observation heuristic.
        if command.starts_with("jobs.") || command.ends_with(".job") || pending(&result, 0) {
            return Ok(());
        }
        let readonly = call.name == "read_tool_result"
            || method == "engine.commands"
            || matches!(
                method,
                "ui.inspect" | "ui.elements" | "ui.docking" | "ui.menu.list"
            )
            || [".inspect", ".list", ".describe", ".search", ".query"]
                .iter()
                .any(|suffix| command.ends_with(suffix));
        if !readonly && !failed {
            self.observations.clear();
            self.recover = false;
            self.finish = false;
            return Ok(());
        }
        let key = serde_json::to_string(&(&call.name, args))
            .map_err(|_| "Cannot compare tool observations")?;
        let bytes = serde_json::to_vec(&result).map_err(|_| "Cannot compare tool observations")?;
        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        let fingerprint = hasher.finish();
        let (old, count) = self.observations.entry(key).or_insert((fingerprint, 0));
        if *old != fingerprint {
            *old = fingerprint;
            *count = 0;
            self.recover = false;
        }
        *count = count.saturating_add(1);
        if *count >= 3 {
            self.recover = true;
        }
        if *count >= 4 {
            self.finish = true;
        }
        Ok(())
    }
}

fn pending(value: &Value, depth: usize) -> bool {
    if depth > 4 {
        return false;
    }
    match value {
        Value::Object(values) => values.iter().any(|(key, value)| {
            (key == "status" && matches!(value.as_str(), Some("running" | "pending" | "queued")))
                || (key == "pending" && value == true)
                || (key == "finished" && value == false)
                || pending(value, depth + 1)
        }),
        Value::Array(values) => values
            .iter()
            .take(256)
            .any(|value| pending(value, depth + 1)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn call(command: &str) -> ToolCall {
        ToolCall::new(
            "editor",
            json!({"method":"engine.execute","params_json":json!({"command":command,"params":{}}).to_string()}),
        )
    }
    #[test]
    fn successful_mutations_and_pending_jobs_are_not_deduplicated() {
        for (command, result) in [
            ("entity.nudge", json!({"ok":true})),
            ("jobs.list", json!({"ok":true})),
            (
                "render.inspect",
                json!({"ok":true,"result":{"status":"running"}}),
            ),
        ] {
            let mut p = Progress::default();
            for _ in 0..12 {
                p.observe(&call(command), &result).unwrap();
            }
            assert!(!p.recover() && !p.finish(), "{command}");
        }
    }
    #[test]
    fn changed_results_and_successful_edits_invalidate_the_loop_observation() {
        let mut p = Progress::default();
        for _ in 0..3 {
            p.observe(
                &call("document.inspect"),
                &json!({"ok":true,"result":{"revision":5}}),
            )
            .unwrap();
        }
        assert!(p.recover());
        p.observe(
            &call("document.inspect"),
            &json!({"ok":true,"result":{"revision":6}}),
        )
        .unwrap();
        assert!(!p.recover());
        p.observe(&call("entity.rename"), &json!({"ok":true}))
            .unwrap();
        assert!(!p.finish());
    }
}
