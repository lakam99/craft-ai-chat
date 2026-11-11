//! Product-independent capabilities and a bounded, stateless chat protocol. No I/O or UI.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod bounds;
pub mod editor;
pub mod protocol;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const DEFAULT_MODEL: &str = "gpt-6.1-sol";
pub const MAX_BODY: usize = 16 * 1024 * 1024;
pub const MAX_REPLY: usize = 8 * 1024 * 1024;

/// Store these values in the host's ordinary editor preferences. Never store a key here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Preferences {
    pub model: String,
    pub attach_frames: bool,
    pub remember_key: bool,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            model: DEFAULT_MODEL.into(),
            attach_frames: true,
            remember_key: true,
        }
    }
}
impl Preferences {
    pub fn valid_model(model: &str) -> bool {
        !model.is_empty()
            && model.len() <= 128
            && model
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-._/".contains(&c))
    }
}

/// Host-owned tool schemas: image editors, documents, audio and 3D tools use the same contract.
#[derive(Clone, Debug)]
pub struct HostInfo {
    pub id: String,
    pub name: String,
    pub instructions: String,
    pub tools: Vec<Value>,
    pub automatic_inputs: Vec<ToolCall>,
    pub attachment_label: String,
    pub welcome: String,
}
impl HostInfo {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty()
            || self.id.len() > 128
            || self.name.len() > 256
            || self.instructions.len() > 32_768
            || self.tools.len() > 64
            || self.automatic_inputs.len() > 8
            || self.attachment_label.len() > 512
            || self.welcome.len() > 2048
        {
            return Err("The host's AI capabilities exceed the plugin limits".into());
        }
        let mut names = std::collections::HashSet::new();
        for tool in &self.tools {
            bounds::value_size(tool, 256 * 1024)?;
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .ok_or("A host tool has no name")?;
            if name.is_empty()
                || name.len() > 64
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                || !names.insert(name)
                || tool.get("type").and_then(Value::as_str) != Some("function")
                || !tool.get("parameters").is_some_and(Value::is_object)
            {
                return Err("The host advertises an invalid or duplicate tool".into());
            }
        }
        for call in &self.automatic_inputs {
            self.validate_call(call)?;
        }
        bounds::serialized_size(&self.tools, 256 * 1024)?;
        Ok(())
    }
    pub fn validate_call(&self, call: &ToolCall) -> Result<(), String> {
        if !self
            .tools
            .iter()
            .any(|t| t.get("name").and_then(Value::as_str) == Some(call.name.as_str()))
        {
            return Err("The host has not advertised this AI tool".into());
        }
        if !call.arguments.is_object()
            || bounds::value_size(&call.arguments, 64 * 1024)? > 64 * 1024
        {
            return Err("Tool arguments must be a JSON object no larger than 64 KiB".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    pub arguments: Value,
}
impl ToolCall {
    pub fn new(name: impl Into<String>, arguments: Value) -> Self {
        Self {
            name: name.into(),
            arguments,
        }
    }
}

pub fn function(name: &str, description: &str, parameters: Value) -> Value {
    json!({"type":"function","name":name,"description":description,"strict":true,"parameters":parameters})
}

pub fn validate_key(key: &str) -> Result<(), String> {
    if key.len() < 10 || key.len() > 512 || !key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("Enter a valid OpenAI API key (no spaces or line breaks)".into());
    }
    Ok(())
}
