use crate::{Host, HostInfo, ToolCall, core};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
#[cfg(test)]
mod tests;

pub(super) struct Batch<T> {
    calls: Vec<ToolCall>,
    pub completed: usize,
    results: Vec<Value>,
    tool: Option<T>,
    deadline: Option<Instant>,
}
impl<T> Batch<T> {
    pub fn new(info: &HostInfo, args: &Value) -> Result<Self, String> {
        let batch = core::batch::parse(info, args)?;
        Ok(Self {
            calls: batch.calls,
            completed: 0,
            results: vec![],
            tool: None,
            deadline: None,
        })
    }
    pub fn total(&self) -> usize {
        self.calls.len()
    }
    pub fn finish(&self, error: Option<&str>) -> Value {
        let mut value = json!({"ok":error.is_none(),"completed":self.completed,"total":self.total(),"remaining":self.total().saturating_sub(self.completed),"results":self.results,"atomic":false});
        if let Some(error) = error {
            value["error"] = json!(error);
        }
        value
    }
    pub fn poll<H: Host<Tool = T>>(&mut self, host: &mut H) -> Result<Option<Value>, String> {
        let Some(call) = self.calls.get(self.completed) else {
            return Ok(Some(self.finish(None)));
        };
        let result = (|| {
            host.info().validate_call(call)?;
            if self.tool.is_none() {
                self.deadline = Some(Instant::now() + Duration::from_secs(30));
                self.tool = Some(host.start_tool(call)?);
            }
            if self.deadline.is_some_and(|d| Instant::now() > d) {
                return Err("Batch command timed out; inspect the editor before retrying".into());
            }
            host.poll_tool(self.tool.as_mut().ok_or("Cannot start batch command")?)
        })();
        let result = match result {
            Ok(None) => return Ok(None),
            Ok(Some(value)) => value,
            Err(error) => {
                return Ok(Some(self.finish(Some(&format!(
                    "Command {} failed: {error}",
                    self.completed + 1
                )))));
            }
        };
        let result = if let Some(text) = result.as_str() {
            serde_json::from_str(text).unwrap_or(result)
        } else {
            result
        };
        let failed = result["ok"] == false || result.get("error").is_some_and(|v| !v.is_null());
        // Large query outputs must not expand a 64-command batch into an unbounded context.
        let stored = if core::bounds::value_size(&result, 8 * 1024).is_ok() {
            result.clone()
        } else {
            json!({"ok":!failed,"outputOmitted":true,"note":"Large output omitted; query this command separately if needed."})
        };
        self.results
            .push(json!({"index":self.completed+1,"result":stored}));
        if failed {
            return Ok(Some(self.finish(Some(&format!(
                "Command {} failed; later commands were not run",
                self.completed + 1
            )))));
        }
        self.completed += 1;
        self.tool = None;
        self.deadline = None;
        if self.completed == self.total() {
            Ok(Some(self.finish(None)))
        } else {
            Ok(None)
        }
    }
}
