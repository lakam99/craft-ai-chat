//! Presentation state is separate from protocol history. Tool JSON never becomes a chat message.
use crate::ToolCall;
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ActivityState {
    Running,
    Done,
    Failed,
    Stopped,
}
impl ActivityState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "Working",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Stopped => "Stopped",
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Activity {
    pub title: String,
    pub state: ActivityState,
    pub detail: String,
}
pub(crate) enum Content {
    Message {
        role: String,
        text: String,
        markdown: Option<String>,
    },
    Activity(Activity),
}
pub(crate) struct Entry {
    pub id: u64,
    pub content: Content,
}
#[derive(Default)]
pub(crate) struct Conversation {
    pub entries: Vec<Entry>,
    // Preserve the public transcript API and automation format, containing only readable text.
    pub transcript: Vec<(String, String)>,
    next_id: u64,
}
impl Conversation {
    pub fn clear(&mut self) {
        self.entries.clear();
        self.transcript.clear();
        // IDs remain monotonic so a delayed completion cannot update a newer entry.
    }
    fn push(&mut self, content: Content, role: &str, text: String) -> u64 {
        if self.entries.len() >= 128 {
            self.entries.remove(0);
            self.transcript.remove(0);
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.entries.push(Entry { id, content });
        self.transcript.push((role.into(), text));
        id
    }
    pub fn message(&mut self, role: &str, text: String) {
        let text: String = text.chars().take(32_768).collect();
        let markdown = if role == "Assistant" || role == "You" {
            prepare_markdown(&text)
        } else {
            None
        };
        self.push(
            Content::Message {
                role: role.into(),
                text: text.clone(),
                markdown,
            },
            role,
            text,
        );
    }
    pub fn begin(&mut self, title: String) -> u64 {
        let title: String = title.chars().take(160).collect();
        let activity = Activity {
            title: title.clone(),
            state: ActivityState::Running,
            detail: String::new(),
        };
        self.push(
            Content::Activity(activity),
            "Tool",
            format!("{title} — Working"),
        )
    }
    pub fn finish(&mut self, id: u64, state: ActivityState, detail: String) {
        if let Some((index, entry)) = self
            .entries
            .iter_mut()
            .enumerate()
            .find(|(_, entry)| entry.id == id)
            && let Content::Activity(activity) = &mut entry.content
        {
            activity.state = state;
            activity.detail = detail.chars().take(512).collect();
            if let Some(row) = self.transcript.get_mut(index) {
                row.1 = format!(
                    "{} — {}{}{}",
                    activity.title,
                    state.label(),
                    if activity.detail.is_empty() { "" } else { ": " },
                    activity.detail
                );
            }
        }
    }
    pub fn interrupt(&mut self, state: ActivityState, detail: &str) {
        let ids: Vec<_> = self
            .entries
            .iter()
            .filter_map(|entry| match &entry.content {
                Content::Activity(activity) if activity.state == ActivityState::Running => {
                    Some(entry.id)
                }
                _ => None,
            })
            .collect();
        for id in ids {
            self.finish(id, state, detail.into());
        }
    }
}

/// Products can resolve this ID against their command registry to show the actual menu label.
pub fn command_id(call: &ToolCall) -> Option<String> {
    if !matches!(
        call.arguments.get("method")?.as_str()?,
        "engine.execute" | "ui.menu.invoke"
    ) {
        return None;
    }
    let encoded = call.arguments.get("params_json")?.as_str()?;
    if encoded.len() > 64 * 1024 {
        return None;
    }
    let params: Value = serde_json::from_str(encoded).ok()?;
    params
        .get("command")
        .or_else(|| params.get("id"))?
        .as_str()
        .map(str::to_owned)
}
pub fn tool_label(call: &ToolCall) -> String {
    if call.name == "video_frame" {
        return call
            .arguments
            .get("monitor")
            .and_then(Value::as_str)
            .map_or_else(
                || "Capture active view".into(),
                |view| format!("Capture {view} frame"),
            );
    }
    let method = call
        .arguments
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or(&call.name);
    match method {
        "engine.commands" | "ui.menu.list" => "Discover editor commands".into(),
        "ui.inspect" | "ui.elements" => "Inspect editor state".into(),
        "ui.click" => "Click editor control".into(),
        "ui.drag" => "Drag editor control".into(),
        "ui.key" => "Use keyboard shortcut".into(),
        "ui.type" => "Enter text".into(),
        "ui.focus" => "Focus editor control".into(),
        "ui.playback" => "Control playback".into(),
        "ui.panel.show" => "Open editor panel".into(),
        "ui.panel.close" => "Close editor panel".into(),
        "engine.execute" | "ui.menu.invoke" => {
            command_id(call).map_or_else(|| "Run editor command".into(), |id| humanize(&id))
        }
        _ => humanize(method),
    }
}
fn humanize(id: &str) -> String {
    let mut text = String::new();
    for c in id.chars().take(120) {
        if matches!(c, '_' | '.' | '-') {
            text.push(' ');
        } else {
            if c.is_uppercase() && !text.is_empty() && !text.ends_with(' ') {
                text.push(' ');
            }
            if text.is_empty() {
                text.extend(c.to_uppercase());
            } else {
                text.extend(c.to_lowercase());
            }
        }
    }
    if text.trim().is_empty() {
        "Use editor tool".into()
    } else {
        text
    }
}

pub(crate) fn outcome(value: &Value) -> (ActivityState, String) {
    let parsed;
    let value = if let Some(text) = value.as_str() {
        if text.len() <= 8 * 1024 * 1024 {
            parsed = serde_json::from_str::<Value>(text).ok();
            parsed.as_ref().unwrap_or(value)
        } else {
            value
        }
    } else {
        value
    };
    if value.get("ok").and_then(Value::as_bool) == Some(false) || value.get("error").is_some() {
        let detail = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("The editor could not complete this action.");
        return (ActivityState::Failed, detail.chars().take(512).collect());
    }
    if let Some(parts) = value.as_array()
        && parts
            .iter()
            .any(|p| p.get("type").and_then(Value::as_str) == Some("input_image"))
    {
        return (ActivityState::Done, "Frame attached".into());
    }
    (ActivityState::Done, String::new())
}

/// Replies are untrusted: never let Markdown load local/remote images or open non-web URI schemes.
/// Bound nesting before entering the third-party renderer; deep content stays selectable plain text.
fn prepare_markdown(text: &str) -> Option<String> {
    use pulldown_cmark::{Event, Options, Parser, Tag};
    let mut depth = 0usize;
    let mut omitted = Vec::new();
    for (count, (event, range)) in Parser::new_ext(
        text,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
    )
    .into_offset_iter()
    .enumerate()
    {
        if count > 8192 {
            return None;
        }
        match event {
            Event::Start(Tag::Image { .. }) => {
                omitted.push((range, "(image)"));
                depth = depth.saturating_add(1);
            }
            Event::Start(Tag::Link { dest_url, .. })
                if !(dest_url.starts_with("https://")
                    || dest_url.starts_with("http://")
                    || dest_url.starts_with('#')) =>
            {
                omitted.push((range, "(link)"));
                depth = depth.saturating_add(1);
            }
            Event::Html(_) | Event::InlineHtml(_) => omitted.push((range, " (HTML omitted) ")),
            Event::Start(_) => depth = depth.saturating_add(1),
            Event::End(_) => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth > 32 {
            return None;
        }
    }
    let mut result = String::new();
    let mut cursor = 0;
    omitted.sort_by_key(|(range, _)| range.start);
    for (range, replacement) in omitted {
        if range.start < cursor {
            continue;
        }
        result.push_str(text.get(cursor..range.start)?);
        result.push_str(replacement);
        cursor = range.end;
    }
    result.push_str(text.get(cursor..)?);
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn tool_results_never_enter_the_readable_transcript_as_json() {
        let mut chat = Conversation::default();
        let id = chat.begin("Set project notes".into());
        let (state, detail) = outcome(&Value::String(
            json!({"ok":true,"result":{"notes":"raw internal data"}}).to_string(),
        ));
        chat.finish(id, state, detail);
        assert_eq!(
            chat.transcript,
            vec![("Tool".into(), "Set project notes — Done".into())]
        );
        assert!(!format!("{:?}", chat.transcript).contains("raw internal data"));
    }
    #[test]
    fn failures_and_frames_have_readable_states() {
        assert_eq!(
            outcome(&json!("{\"ok\":false,\"error\":\"Layer is locked\"}")),
            (ActivityState::Failed, "Layer is locked".into())
        );
        assert_eq!(
            outcome(&json!([{"type":"input_image","image_url":"data:image/png;base64,AAAA"}])),
            (ActivityState::Done, "Frame attached".into())
        );
    }
    #[test]
    fn truncation_and_clear_do_not_retarget_delayed_activity_updates() {
        let mut chat = Conversation::default();
        let old = chat.begin("Old edit".into());
        for _ in 0..150 {
            chat.message("You", "Message".into());
        }
        chat.finish(old, ActivityState::Done, String::new());
        assert_eq!(chat.entries.len(), 128);
        assert_eq!(chat.entries.len(), chat.transcript.len());
        chat.clear();
        let new = chat.begin("New edit".into());
        chat.finish(old, ActivityState::Failed, "Old error".into());
        assert_ne!(old, new);
        assert!(chat.transcript[0].1.contains("Working"));
    }
    #[test]
    fn markdown_keeps_formatting_without_loading_images_or_local_links() {
        let formatted = "## Updated\n\n**Done**. [Guide](https://example.com)\n\n- First\n- Second\n\n```rust\nlet x = 1;\n```";
        assert_eq!(prepare_markdown(formatted).as_deref(), Some(formatted));
        for text in [
            "![secret](file:///private/key)",
            "![tracking](https://example.com/pixel)",
            "[Run](javascript:alert)",
            "<img src=x>",
        ] {
            let safe = prepare_markdown(text).unwrap();
            assert!(
                !safe.contains("file:")
                    && !safe.contains("javascript:")
                    && !safe.contains("https://")
                    && !safe.contains("<img"),
                "{safe}"
            );
        }
        let safe = prepare_markdown("**Keep bold** ![image](https://example.com/pixel)").unwrap();
        assert!(safe.contains("**Keep bold**"));
        assert!(!safe.contains("https://"));
        assert!(prepare_markdown(&format!("{} nested", "> ".repeat(80))).is_none());
    }
}
