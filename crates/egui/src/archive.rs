use crate::*;
#[derive(Default)]
pub(super) struct Archive {
    key: Option<(std::path::PathBuf, String, String)>,
    store: Option<craft_ai_chat_native::history::Store>,
    loading: bool,
    pub message: String,
}
impl Archive {
    pub fn loading(&self) -> bool {
        self.loading
    }
}
impl<T> Panel<T> {
    pub(super) fn sync_archive<H: Host<Tool = T>>(&mut self, host: &H, ctx: &egui::Context) {
        let key = self
            .profile
            .as_ref()
            .filter(|_| self.preferences.remember_history)
            .map(|p| {
                (
                    p.preferences_file.clone(),
                    host.info().id.clone(),
                    host.document_key()
                        .unwrap_or_else(|| "unsaved-session".into()),
                )
            });
        if self.archive.key != key {
            self.persist();
            let changed = self
                .archive
                .key
                .as_ref()
                .zip(key.as_ref())
                .is_some_and(|(old, new)| old != new);
            if key.is_none()
                && let Some(store) = &mut self.archive.store
            {
                store.queue(json!({"version":1,"model":"","history":[],"transcript":[]}));
            }
            self.archive = Archive {
                key: key.clone(),
                ..Default::default()
            };
            if changed
                && self
                    .history_instance
                    .as_ref()
                    .is_none_or(|instance| !Arc::ptr_eq(instance, &host.instance()))
            {
                self.stop();
                self.history.clear();
                self.chat.clear();
                self.history_instance = None;
                self.batch_progress = None;
            }
            if let Some((path, id, document)) = key {
                match craft_ai_chat_native::history::Store::open(&path, &id, &document, wake(ctx)) {
                    Ok(store) => {
                        self.archive.store = Some(store);
                        self.archive.loading = true;
                    }
                    Err(error) => self.archive.message = error,
                }
            }
        }
        let event = self
            .archive
            .store
            .as_ref()
            .and_then(|s| s.events.try_recv().ok());
        if let Some(event) = event {
            self.archive.loading = false;
            match event {
                Ok(Some(value)) => {
                    if self.history.is_empty() && self.chat.transcript.is_empty() {
                        if let Err(error) = self.restore_archive(value) {
                            self.archive.message = error;
                        }
                        self.history_instance = Some(host.instance());
                    }
                }
                Ok(None) => {
                    if !self.history.is_empty() {
                        self.persist();
                    }
                }
                Err(error) => self.archive.message = error,
            }
        }
        if self.archive.loading {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
    pub(super) fn restore_archive(&mut self, value: Value) -> Result<(), String> {
        if value["version"] != 1 {
            return Err("Unsupported chat history format".into());
        }
        core::bounds::value_size(&value, craft_ai_chat_native::history::MAX_ARCHIVE)?;
        let history: Vec<Value> = serde_json::from_value(value["history"].clone())
            .map_err(|_| "Invalid saved conversation context")?;
        let rows: Vec<(String, String)> = serde_json::from_value(value["transcript"].clone())
            .map_err(|_| "Invalid saved chat transcript")?;
        if history.len() > 512
            || rows.len() > 128
            || rows.iter().any(|(role, text)| {
                !matches!(role.as_str(), "You" | "Assistant" | "Tool" | "Notice")
                    || text.len() > 131072
            })
        {
            return Err("Saved chat history exceeds its limits".into());
        }
        if history.iter().any(|item| {
            if let Some(role) = item["role"].as_str() {
                !matches!(role, "user" | "assistant")
            } else {
                !matches!(
                    item["type"].as_str(),
                    Some("function_call" | "function_call_output" | "reasoning" | "compaction")
                )
            }
        }) {
            return Err("Saved chat history contains unsupported message roles".into());
        }
        self.history = history;
        core::history::omit_media(&mut self.history);
        core::history::close_pending(
            &mut self.history,
            "The previous editor session ended during a request",
        );
        self.chat.restore(rows);
        self.history_model = value["model"].as_str().unwrap_or_default().into();
        Ok(())
    }
    pub(super) fn persist(&mut self) {
        if self.archive.loading || self.archive.store.is_none() {
            return;
        }
        let mut history = self.history.clone();
        if let Some(progress) = &self.batch_progress {
            history.push(progress.clone());
        }
        core::history::omit_media(&mut history);
        history.retain(|item| item["type"] != "reasoning");
        let mut value = json!({"version":1,"model":self.history_model,"history":history,"transcript":self.chat.transcript});
        if core::bounds::value_size(&value, craft_ai_chat_native::history::MAX_ARCHIVE).is_err() {
            self.archive.message =
                "Chat history is too large to save; compact it by sending a follow-up".into();
            return;
        }
        // A key pasted into chat must not escape into this archive either.
        if !self.key.is_empty() {
            redact(&mut value, &self.key);
        }
        if let Some(store) = &mut self.archive.store {
            store.queue(value);
        }
    }
}
fn redact(value: &mut Value, key: &str) {
    let mut stack = vec![value];
    while let Some(value) = stack.pop() {
        match value {
            Value::String(text) => {
                *text = text.replace(key, "[API key omitted]");
            }
            Value::Array(a) => stack.extend(a.iter_mut()),
            Value::Object(o) => stack.extend(o.values_mut()),
            _ => {}
        }
    }
}
