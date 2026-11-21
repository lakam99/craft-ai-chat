//! Native component implementation. Only the host adapter can execute editor tools.
use crate::{
    CredentialProfile, Host, HostInfo, Panel, Preferences, ToolCall, conversation::Content,
};
use craft_ai_chat_core::runtime::{self, Input, Node, Output, Snapshot, TextStyle as Style, View};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, mpsc},
    time::Duration,
};

struct Bridge {
    snapshot: Snapshot,
    instance: Arc<()>,
    next: u64,
    replies: BTreeMap<u64, Result<Value, String>>,
    outgoing: Vec<Output>,
    submitted: Option<runtime::Receipt>,
}
impl Bridge {
    fn sync(&mut self, snapshot: Snapshot) -> Result<(), String> {
        snapshot.validate()?;
        if self.snapshot.epoch != snapshot.epoch {
            self.instance = Arc::new(());
            self.replies.clear();
            self.submitted = None;
        }
        self.snapshot = snapshot;
        Ok(())
    }
}
impl Host for Bridge {
    type Tool = u64;
    fn info(&self) -> &HostInfo {
        &self.snapshot.info
    }
    fn preferences(&self) -> Preferences {
        self.snapshot.preferences.clone()
    }
    fn instance(&self) -> Arc<()> {
        self.instance.clone()
    }
    fn document_key(&self) -> Option<String> {
        self.snapshot.document.clone()
    }
    fn credential_profile(&self) -> Option<CredentialProfile> {
        self.snapshot.profile.as_ref().map(|p| CredentialProfile {
            preferences_file: p.preferences_file.clone(),
            service: p.service.clone(),
        })
    }
    fn chat_is_open(&self) -> bool {
        self.snapshot.open
    }
    fn external_guidance(&self) -> bool {
        true
    }
    fn start_tool(&mut self, call: &ToolCall) -> Result<u64, String> {
        self.info().validate_call(call)?;
        self.next = self
            .next
            .checked_add(1)
            .ok_or("Chat request IDs exhausted")?;
        if self.outgoing.len() >= 64 {
            return Err("Chat bridge is busy".into());
        }
        self.outgoing.push(Output::Tool {
            epoch: self.snapshot.epoch,
            id: self.next,
            call: call.clone(),
        });
        Ok(self.next)
    }
    fn poll_tool(&mut self, id: &mut u64) -> Result<Option<Value>, String> {
        self.replies.remove(id).transpose()
    }
    fn set_preference(&mut self, key: &str, value: Value) -> Result<(), String> {
        self.outgoing.push(Output::Preference {
            epoch: self.snapshot.epoch,
            key: key.into(),
            value,
        });
        Ok(())
    }
    fn open_preferences(&mut self) -> Result<(), String> {
        self.outgoing.push(Output::OpenPreferences);
        Ok(())
    }
    fn widget(&mut self, _: &str, _: &egui::Response, _: &str) {}
    fn area(&mut self, _: &str, _: egui::Rect, _: &str) {}
}

fn text(id: &str, value: impl Into<String>, style: Style) -> Node {
    Node::Text {
        id: id.into(),
        text: value.into(),
        style,
    }
}
fn button(id: &str, label: &str, enabled: bool) -> Node {
    Node::Button {
        id: id.into(),
        label: label.into(),
        enabled,
        primary: id == "ai.send",
    }
}
impl Panel<u64> {
    fn remote_view(&self, host: &Bridge) -> View {
        let idle = !self.busy();
        let unlocked = idle && !self.vault.busy();
        let mut settings = vec![
            text("ai.key.title", "OpenAI connection", Style::Heading),
            Node::Input {
                id: "ai.key".into(),
                label: "OpenAI API key".into(),
                value: String::new(),
                password: true,
                multiline: false,
                enabled: unlocked,
            },
            Node::Row {
                children: vec![
                    button(
                        "ai.saveKey",
                        "Save key securely",
                        unlocked && self.preferences.remember_key,
                    ),
                    button(
                        "ai.loadKey",
                        "Load saved key",
                        unlocked && self.preferences.remember_key,
                    ),
                    button("ai.forgetKey", "Forget key", unlocked),
                ],
            },
            text(
                "ai.key.help",
                "Keys stay in the OS credential store. Messages and attached images go to OpenAI using your API account.",
                Style::Muted,
            ),
        ];
        if !self.vault.message.is_empty() {
            settings.push(text("ai.key.status", &self.vault.message, Style::Muted));
        }
        if !self.archive.message.is_empty() {
            settings.push(text(
                "ai.history.status",
                &self.archive.message,
                Style::Warning,
            ));
        }
        let mut top = vec![
            Node::Row {
                children: vec![
                    text("ai.title", "Assistant", Style::Heading),
                    button("ai.clear", "New chat", idle),
                    button("ai.settings", "Preferences…", true),
                ],
            },
            text("ai.model", &self.preferences.model, Style::Muted),
            button(
                "ai.connection",
                if self.has_key() {
                    "API key ready"
                } else {
                    "Set up API key"
                },
                true,
            ),
        ];
        if self.connection_open {
            let mut connection = settings.clone();
            if host
                .snapshot
                .capabilities
                .iter()
                .any(|c| c == runtime::COMPONENT_ACTIONS)
            {
                connection.push(button(
                    "ai.connection.close",
                    "Close connection settings",
                    true,
                ));
            }
            top.push(Node::Card {
                id: "ai.connection.settings".into(),
                accent: false,
                children: connection,
            });
        }
        top.push(Node::Separator);
        let mut body = Vec::new();
        if self.chat.entries.is_empty() {
            body.push(text(
                "ai.welcome.title",
                "What would you like to create?",
                Style::Heading,
            ));
            body.push(text("ai.welcome", &host.info().welcome, Style::Body));
            body.push(text(
                "ai.welcome.help",
                "Describe an edit or ask a question. Editor actions use the normal tools and Undo.",
                Style::Muted,
            ));
        }
        for entry in &self.chat.entries {
            let (accent, children) = match &entry.content {
                Content::Message {
                    role,
                    text: raw,
                    markdown,
                } => {
                    let mut nodes = vec![text(
                        &format!("ai.message.{}.role", entry.id),
                        role,
                        if role == "Notice" {
                            Style::Warning
                        } else {
                            Style::Muted
                        },
                    )];
                    nodes.push(text(
                        &format!("ai.message.{}.text", entry.id),
                        markdown.as_ref().unwrap_or(raw),
                        if markdown.is_some() {
                            Style::Markdown
                        } else {
                            Style::Body
                        },
                    ));
                    if role == "Assistant" {
                        nodes.push(button(
                            &format!("ai.message.{}.copy", entry.id),
                            "Copy",
                            true,
                        ));
                    }
                    (role == "You", nodes)
                }
                Content::Activity(a) => (
                    false,
                    vec![
                        text(
                            &format!("ai.activity.{}", entry.id),
                            format!("{} · {}", a.state.label(), a.title),
                            Style::Muted,
                        ),
                        text(
                            &format!("ai.activity.{}.detail", entry.id),
                            &a.detail,
                            Style::Body,
                        ),
                    ],
                ),
            };
            body.push(Node::Card {
                id: format!("ai.message.{}", entry.id),
                accent,
                children,
            });
        }
        if !idle {
            body.push(text("ai.working", "Working in the editor…", Style::Muted));
        }
        let bottom = vec![
            Node::Separator,
            Node::Input {
                id: "ai.prompt".into(),
                label: "Ask about your work, or describe an edit…".into(),
                value: self.prompt.clone(),
                password: false,
                multiline: true,
                enabled: idle,
            },
            Node::Toggle {
                id: "ai.attach".into(),
                label: host.info().attachment_label.clone(),
                value: self.preferences.attach_frames,
                enabled: idle,
            },
            Node::Row {
                children: vec![
                    if idle {
                        button(
                            "ai.send",
                            "Send",
                            unlocked && !self.prompt.trim().is_empty(),
                        )
                    } else {
                        button("ai.stop", "Stop", true)
                    },
                    text(
                        "ai.keyboard",
                        "Enter to send · Shift+Enter for a new line",
                        Style::Muted,
                    ),
                ],
            },
        ];
        View {
            top,
            body,
            bottom,
            settings,
        }
    }
    fn remote_action(
        &mut self,
        host: &mut Bridge,
        ctx: &egui::Context,
        id: &str,
        value: Value,
    ) -> Result<(), String> {
        match id {
            "ai.connection.close" => {
                self.connection_open = false;
                Ok(())
            }
            "ai.prompt" => {
                self.prompt = value
                    .as_str()
                    .filter(|s| s.len() <= 32768)
                    .ok_or("Invalid chat draft")?
                    .into();
                Ok(())
            }
            "ai.key" => {
                self.key = value
                    .as_str()
                    .filter(|s| s.len() <= 512)
                    .ok_or("Invalid key input")?
                    .into();
                Ok(())
            }
            "ai.send" => {
                if let Some(prompt) = value.as_str() {
                    self.prompt = prompt.to_owned();
                }
                self.command(host, ctx, id)
                    .ok_or("Unknown chat action")?
                    .map(|_| ())
            }
            "ai.attach" => host.set_preference(
                "ai.attachFrames",
                json!(value.as_bool().ok_or("Invalid attachment preference")?),
            ),
            _ => self
                .command(host, ctx, id)
                .ok_or("Unknown chat action")?
                .map(|_| ()),
        }
    }
}

/// Run only with private inherited stdio pipes; no socket or inbound network service.
pub fn run() -> Result<(), String> {
    let (sender, receive) = mpsc::sync_channel(16);
    std::thread::Builder::new()
        .name("chat-pipe-reader".into())
        .spawn(move || {
            let work = || {
                let mut input = std::io::stdin().lock();
                loop {
                    match runtime::read::<Input>(&mut input) {
                        Ok(Some(packet)) => {
                            if sender.send(Ok(packet)).is_err() {
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            let _ = sender.send(Err(error));
                            break;
                        }
                    }
                }
            };
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
        })
        .map_err(|e| format!("Cannot start chat input worker: {e}"))?;
    let mut output = std::io::stdout().lock();
    runtime::write(
        &mut output,
        &Output::Ready {
            protocol: runtime::PROTOCOL,
            version: env!("CARGO_PKG_VERSION").into(),
            capabilities: vec![runtime::SEND_RECEIPTS.into()],
        },
    )?;
    let ctx = egui::Context::default();
    let mut host: Option<Bridge> = None;
    let mut panel = Panel::<u64>::default();
    let mut previous = Vec::new();
    loop {
        // Drain viewport repaint commands issued by shared worker wakeups. This process
        // has no window/event loop to consume them, so an undrained context grows forever.
        ctx.run_ui(egui::RawInput::default(), |_| {})
            .drop_without_applying_deltas();
        match receive.recv_timeout(Duration::from_millis(40)) {
            Ok(Ok(Input::Shutdown)) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(Err(error)) => return Err(error),
            Ok(Ok(Input::Sync { snapshot })) => {
                snapshot.validate()?;
                if let Some(host) = &mut host {
                    host.sync(snapshot)?;
                } else {
                    host = Some(Bridge {
                        snapshot,
                        instance: Arc::new(()),
                        next: 0,
                        replies: BTreeMap::new(),
                        outgoing: Vec::new(),
                        submitted: None,
                    });
                }
            }
            Ok(Ok(Input::Action {
                serial,
                snapshot,
                id,
                value,
            })) => {
                snapshot.validate()?;
                if host.is_none() {
                    host = Some(Bridge {
                        snapshot: snapshot.clone(),
                        instance: Arc::new(()),
                        next: 0,
                        replies: BTreeMap::new(),
                        outgoing: Vec::new(),
                        submitted: None,
                    });
                }
                if let Some(host) = &mut host {
                    host.sync(snapshot)?;
                    let result = panel.remote_action(host, &ctx, &id, value);
                    if id == "ai.send" {
                        host.submitted = Some(runtime::Receipt {
                            serial,
                            accepted: result.is_ok(),
                        });
                    }
                    if let Err(error) = result {
                        if id == "ai.send" && crate::core::validate_key(panel.key.trim()).is_err() {
                            panel.connection_open = true;
                        }
                        panel.message = error.clone();
                        panel.record("Notice", error);
                    }
                }
            }
            Ok(Ok(Input::Reply { epoch, id, result })) => {
                if let Some(host) = &mut host
                    && epoch == host.snapshot.epoch
                    && host.replies.len() < 64
                {
                    host.replies.insert(id, result);
                }
            }
            Ok(Ok(Input::Failure { message })) => {
                panel.fail(&message);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some(host) = &mut host {
            panel.poll(host, &ctx);
            panel.initialize_vault(&ctx);
            let view = panel.remote_view(host);
            view.validate()?;
            let packet = Output::View {
                epoch: host.snapshot.epoch,
                view,
                status: panel.status(host),
                submitted: host.submitted,
            };
            let encoded = serde_json::to_vec(&packet).map_err(|_| "Cannot encode chat view")?;
            if encoded != previous {
                runtime::write(&mut output, &packet)?;
                previous = encoded;
            }
            // Report Send acceptance before requesting any editor mutation.
            for packet in host.outgoing.drain(..) {
                runtime::write(&mut output, &packet)?;
            }
        }
    }
    panel.stop();
    panel.persist();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bridge() -> Bridge {
        Bridge {
            snapshot: Snapshot {
                protocol: runtime::PROTOCOL,
                epoch: 1,
                info: crate::core::editor::EditorTools {
                    name: "test",
                    methods: &["engine.execute"],
                    views: &[],
                }
                .host_info("test", "Test", "test", "test"),
                preferences: Preferences::default(),
                document: None,
                profile: None,
                open: true,
                capabilities: Vec::new(),
            },
            instance: Arc::new(()),
            next: 0,
            replies: BTreeMap::new(),
            outgoing: vec![],
            submitted: None,
        }
    }
    #[test]
    fn independent_view_owns_layout_without_revealing_key_or_tool_json() {
        let mut panel = Panel::<u64>::default();
        let host = bridge();
        panel.key = "secret-api-key".into();
        panel.record("Assistant", "**Hello** from the component".into());
        let view = panel.remote_view(&host);
        view.validate().unwrap();
        let encoded = serde_json::to_string(&view).unwrap();
        assert!(encoded.contains("markdown"));
        assert!(!encoded.contains("secret-api-key"));
        assert!(!encoded.contains("params_json"));
    }
    #[test]
    fn new_component_controls_are_negotiated_without_breaking_old_hosts() {
        let mut host = bridge();
        let panel = Panel::<u64> {
            connection_open: true,
            ..Default::default()
        };
        assert!(
            !serde_json::to_string(&panel.remote_view(&host))
                .unwrap()
                .contains("ai.connection.close")
        );
        host.snapshot
            .capabilities
            .push(runtime::COMPONENT_ACTIONS.into());
        assert!(
            serde_json::to_string(&panel.remote_view(&host))
                .unwrap()
                .contains("ai.connection.close")
        );
    }
    #[test]
    fn document_epoch_replaces_identity_and_drops_pending_replies() {
        let mut host = bridge();
        let old = host.instance();
        host.replies.insert(1, Ok(json!("old")));
        let mut snapshot = host.snapshot.clone();
        snapshot.epoch = 2;
        host.sync(snapshot).unwrap();
        assert!(!Arc::ptr_eq(&old, &host.instance()));
        assert!(host.replies.is_empty());
    }
    #[test]
    fn walkthrough_tools_are_resolved_by_the_editor_not_the_service() {
        let mut host = bridge();
        host.snapshot.info = crate::core::editor::with_guidance(host.snapshot.info);
        host.snapshot.preferences.attach_frames = false;
        host.snapshot.preferences.remember_history = false;
        let mut panel = Panel::<u64> {
            prompt: "Guide me".into(),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut calls = 0;
        panel.send_with(&mut host,&ctx,move|body| {
            calls+=1;
            if calls==1 {Ok(json!({"status":"completed","output":[{"type":"function_call","call_id":"guide","name":"guide_state","arguments":"{}"}]}))}
            else {assert!(body["input"].to_string().contains("editor-owned-guidance"));Ok(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Ready."}]}]}))}
        }).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut forwarded = false;
        while panel.busy() {
            panel.poll(&mut host, &ctx);
            for output in host.outgoing.drain(..) {
                if let Output::Tool { id, call, .. } = output {
                    assert_eq!(call.name, "guide_state");
                    forwarded = true;
                    host.replies
                        .insert(id, Ok(json!({"state":"editor-owned-guidance"})));
                }
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(forwarded);
        assert!(panel.transcript().iter().any(|(_, text)| text == "Ready."));
    }
}
