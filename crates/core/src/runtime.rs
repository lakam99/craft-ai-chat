//! Versioned native chat bridge. Packets are private pipe data, never log records.
use crate::{HostInfo, Preferences, ToolCall};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::io::{BufRead, Write};

pub const PROTOCOL: u32 = 1;
pub const MAX_PACKET: usize = 20 * 1024 * 1024;
pub const MAX_VIEW: usize = 4 * 1024 * 1024;
pub const SEND_RECEIPTS: &str = "send-receipts";
pub const COMPONENT_ACTIONS: &str = "component-actions";
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct Receipt {
    pub serial: u64,
    pub accepted: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Profile {
    pub preferences_file: std::path::PathBuf,
    pub service: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub protocol: u32,
    pub epoch: u64,
    pub info: HostInfo,
    pub preferences: Preferences,
    pub document: Option<String>,
    pub profile: Option<Profile>,
    pub open: bool,
    #[serde(default)]
    pub capabilities: Vec<String>,
}
impl Snapshot {
    pub fn validate(&self) -> Result<(), String> {
        if self.capabilities.len() > 16 || self.capabilities.iter().any(|v| v.len() > 64) {
            return Err("Invalid host bridge capabilities".into());
        }
        if self.protocol != PROTOCOL {
            return Err("This chat component needs a different editor bridge version".into());
        }
        self.info.validate()?;
        if !Preferences::valid_model(&self.preferences.model)
            || self.document.as_ref().is_some_and(|s| s.len() > 32_768)
            || self.profile.as_ref().is_some_and(|p| {
                p.service.len() > 256 || p.preferences_file.as_os_str().len() > 32_768
            })
        {
            return Err("Invalid chat host profile".into());
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Input {
    Sync {
        snapshot: Snapshot,
    },
    Action {
        #[serde(default)]
        serial: u64,
        snapshot: Snapshot,
        id: String,
        value: Value,
    },
    Reply {
        epoch: u64,
        id: u64,
        result: Result<Value, String>,
    },
    Failure {
        message: String,
    },
    Shutdown,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Output {
    Ready {
        protocol: u32,
        version: String,
        #[serde(default)]
        capabilities: Vec<String>,
    },
    View {
        epoch: u64,
        view: View,
        status: Value,
        #[serde(default)]
        submitted: Option<Receipt>,
    },
    Tool {
        epoch: u64,
        id: u64,
        call: ToolCall,
    },
    Preference {
        epoch: u64,
        key: String,
        value: Value,
    },
    OpenPreferences,
    Error {
        message: String,
    },
}

/// Generic native widgets. The component owns structure, labels, cards and actions.
/// The editor owns focus, accessibility, rendering, and the permitted primitives.
#[derive(Clone, Serialize, Deserialize)]
pub struct View {
    pub top: Vec<Node>,
    pub body: Vec<Node>,
    pub bottom: Vec<Node>,
    pub settings: Vec<Node>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Node {
    Text {
        id: String,
        text: String,
        style: TextStyle,
    },
    Button {
        id: String,
        label: String,
        enabled: bool,
        primary: bool,
    },
    Input {
        id: String,
        label: String,
        value: String,
        password: bool,
        multiline: bool,
        enabled: bool,
    },
    Toggle {
        id: String,
        label: String,
        value: bool,
        enabled: bool,
    },
    Row {
        children: Vec<Node>,
    },
    Card {
        id: String,
        accent: bool,
        children: Vec<Node>,
    },
    Separator,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextStyle {
    Body,
    Heading,
    Muted,
    Markdown,
    Warning,
}

impl View {
    pub fn validate(&self) -> Result<(), String> {
        fn walk(nodes: &[Node], depth: usize, count: &mut usize) -> Result<(), String> {
            if depth > 12 {
                return Err("Chat layout nesting is too deep".into());
            }
            for node in nodes {
                if let Node::Button { id, .. } | Node::Input { id, .. } | Node::Toggle { id, .. } =
                    node
                    && (!id.starts_with("ai.")
                        || id.len() > 256
                        || id.chars().any(char::is_control))
                {
                    return Err("Interactive chat widgets must use bounded ai.* IDs".into());
                }
                *count = count.saturating_add(1);
                if *count > 4096 {
                    return Err("Chat layout is too large".into());
                }
                match node {
                    Node::Row { children } | Node::Card { children, .. } => {
                        walk(children, depth + 1, count)?
                    }
                    Node::Input {
                        value,
                        password: true,
                        ..
                    } if !value.is_empty() => {
                        return Err("A chat view must never expose a saved key".into());
                    }
                    _ => {}
                }
            }
            Ok(())
        }
        crate::bounds::serialized_size(self, MAX_VIEW)?;
        let mut count = 0;
        for nodes in [&self.top, &self.body, &self.bottom, &self.settings] {
            walk(nodes, 0, &mut count)?;
        }
        Ok(())
    }
}

/// Read one bounded newline-delimited packet without first allocating an unbounded line.
pub fn read<T: DeserializeOwned>(reader: &mut impl BufRead) -> Result<Option<T>, String> {
    let mut bytes = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|e| format!("Chat pipe read failed: {e}"))?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err("Truncated chat packet".into())
            };
        }
        let end = available.iter().position(|b| *b == b'\n');
        let take = end.map_or(available.len(), |i| i + 1);
        if bytes.len().saturating_add(take) > MAX_PACKET {
            return Err("Chat packet exceeds the bridge limit".into());
        }
        bytes.extend_from_slice(available.get(..take).ok_or("Invalid chat packet range")?);
        reader.consume(take);
        if end.is_some() {
            break;
        }
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| "Invalid chat bridge packet".into())
}
pub fn write<T: Serialize>(writer: &mut impl Write, packet: &T) -> Result<(), String> {
    crate::bounds::serialized_size(packet, MAX_PACKET - 1)?;
    serde_json::to_writer(&mut *writer, packet)
        .map_err(|_| "Cannot encode chat packet".to_string())?;
    writer
        .write_all(b"\n")
        .and_then(|()| writer.flush())
        .map_err(|e| format!("Chat pipe write failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bridge_rejects_truncation_bad_json_and_oversized_frames() {
        for data in [
            b"{".to_vec(),
            b"garbage\n".to_vec(),
            vec![b'x'; MAX_PACKET + 1],
        ] {
            assert!(read::<Input>(&mut std::io::Cursor::new(data)).is_err());
        }
    }
    #[test]
    fn views_cannot_leak_keys_or_exceed_layout_bounds() {
        let mut view = View {
            top: vec![],
            body: vec![],
            bottom: vec![],
            settings: vec![],
        };
        view.body.push(Node::Input {
            id: "key".into(),
            label: "key".into(),
            value: "secret".into(),
            password: true,
            multiline: false,
            enabled: true,
        });
        assert!(view.validate().is_err());
        view.body = vec![Node::Separator; 4097];
        assert!(view.validate().is_err());
        let mut node = Node::Separator;
        for _ in 0..14 {
            node = Node::Row {
                children: vec![node],
            };
        }
        view.body = vec![node];
        assert!(view.validate().is_err());
    }
}
