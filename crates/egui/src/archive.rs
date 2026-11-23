use crate::*;
#[derive(Default)]
pub(super) struct Archive {
    key: Option<(std::path::PathBuf, String, String)>,
    pub(super) store: Option<craft_ai_chat_native::history::Store>,
    loading: bool,
    pub message: String,
    pub library: Option<craft_ai_chat_native::library::Library>,
    pub browsing: bool,
    pub saved: bool,
    pub query: String,
    pub blocked: bool,
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
                self.goal = None;
                self.goal_mode = false;
                self.queued.clear();
                self.resume_queued = false;
                self.transport = None;
            }
            if let Some((path, id, document)) = key {
                match craft_ai_chat_native::library::Library::open(&path, &id, &document).and_then(
                    |mut library| {
                        let store = library.store(wake(ctx))?;
                        Ok((library, store))
                    },
                ) {
                    Ok((library, store)) => {
                        self.archive.library = Some(library);
                        self.archive.store = Some(store);
                        self.archive.loading = true;
                    }
                    Err(error) => {
                        self.archive.message = error;
                        self.archive.blocked = true;
                    }
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
                            self.archive.blocked = true;
                            self.goal_mode = false;
                        }
                        self.history_instance = Some(host.instance());
                    } else {
                        // Enabling saving (or Save As onto an existing scope) must save the
                        // current chat now, without waiting for another message to be sent.
                        self.persist();
                    }
                }
                Ok(None) => {
                    if !self.history.is_empty() {
                        self.persist();
                    }
                }
                Err(error) => {
                    self.archive.message = error;
                    self.archive.blocked = true;
                }
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
                    Some(
                        "function_call"
                            | "function_call_output"
                            | "reasoning"
                            | "compaction"
                            | core::tool_results::RECORD_TYPE
                    )
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
        self.goal_mode = value["goalMode"].as_bool().unwrap_or(false);
        let queued: Vec<String> =
            serde_json::from_value(value.get("queued").cloned().unwrap_or(json!([])))
                .map_err(|_| "Invalid saved chat interruptions")?;
        if queued.len() > 8 || queued.iter().map(String::len).sum::<usize>() > 32768 {
            return Err("Saved chat interruptions exceed their bounds".into());
        }
        self.queued = queued;
        self.resume_queued = false;
        self.goal = None;
        if !value["goal"].is_null() {
            let goal: core::goal::Goal =
                serde_json::from_value(value["goal"].clone()).map_err(|_| "Invalid saved goal")?;
            let profile = self
                .profile
                .as_ref()
                .ok_or("Saved goal requires a preferences profile")?;
            self.goal = Some(goals::Session::open(profile, &self.host_id, goal, true)?);
        }
        Ok(())
    }
    pub(super) fn persist(&mut self) {
        self.archive.saved = false;
        if self.archive.loading || self.archive.blocked || self.archive.store.is_none() {
            return;
        }
        let mut history = self.history.clone();
        if let Some(progress) = &self.batch_progress {
            history.push(progress.clone());
        }
        core::history::omit_media(&mut history);
        // Responses requires reasoning items alongside their function calls/outputs.
        // Keep the opaque provider state intact; it is never rendered as chat text.
        let goal = self
            .goal
            .as_ref()
            .map(|goal| goals::lock(goal).goal.clone());
        let mut value = json!({"version":1,"model":self.history_model,"history":history,"transcript":self.chat.transcript,"goalMode":self.goal_mode,"goal":goal,"queued":self.queued});
        while core::bounds::value_size(&value, craft_ai_chat_native::history::MAX_ARCHIVE).is_err()
        {
            // Reproducible observations must never prevent saving the user's conversation.
            // Expired detail references report their expiry after reopening.
            let evicted = value["history"].as_array_mut().is_some_and(|history| {
                if let Some(index) = history
                    .iter()
                    .position(|item| item["type"] == core::tool_results::RECORD_TYPE)
                {
                    history.remove(index);
                    true
                } else {
                    false
                }
            });
            if !evicted {
                self.archive.message =
                    "Chat history is too large to save; compact it by sending a follow-up".into();
                return;
            }
        }
        // A key pasted into chat must not escape into this archive either.
        if !self.key.is_empty() {
            redact(&mut value, &self.key);
        }
        if let Some(store) = &mut self.archive.store {
            store.queue(value);
            self.archive.saved = true;
        }
        if let Some(library) = &mut self.archive.library
            && let Some((_, first)) = self.chat.transcript.iter().find(|(role, _)| role == "You")
        {
            let title = if self.key.trim().is_empty() {
                first.clone()
            } else {
                first.replace(self.key.trim(), "[API key omitted]")
            };
            if let Err(error) = library.title(&title) {
                self.archive.message = error;
            }
        }
    }
    pub(super) fn switch_chat<H: Host<Tool = T>>(
        &mut self,
        host: &H,
        ctx: &egui::Context,
        id: Option<&str>,
    ) -> Result<(), String> {
        if self.busy() {
            return Err("Pause the assistant before switching chats".into());
        }
        if self.archive.loading {
            return Err("Wait for chat history to load".into());
        }
        self.persist();
        if let Some(library) = &mut self.archive.library {
            if let Some(id) = id {
                library.select(id)?;
            } else {
                library.create()?;
            }
            // Dropping the old writer drains its last snapshot before another chat opens.
            self.archive.store.take();
            self.archive.store = Some(library.store(wake(ctx))?);
            self.archive.loading = true;
        } else if id.is_some() {
            return Err("Enable local history to reopen saved chats".into());
        }
        self.goal = None;
        self.goal_mode = false;
        self.queued.clear();
        self.resume_queued = false;
        self.transport = None;
        self.guidance.clear();
        self.history.clear();
        self.chat.clear();
        self.markdown_cache = egui_commonmark::CommonMarkCache::default();
        self.message.clear();
        self.history_instance = Some(host.instance());
        self.archive.browsing = false;
        self.archive.blocked = false;
        Ok(())
    }
}
pub(super) fn redact(value: &mut Value, key: &str) {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_observation_records_restore_without_becoming_chat_bubbles() {
        let mut panel = Panel::<()>::default();
        panel.restore_archive(json!({"version":1,"model":"model","history":[
            {"role":"user","content":"Continue the document operation"},
            {"type":core::tool_results::RECORD_TYPE,"call_id":"observed","result":{"entities":[{"id":42}]}}
        ],"transcript":[["You","Continue the document operation"]]})).unwrap();
        assert_eq!(panel.history.len(), 2);
        assert_eq!(panel.transcript().len(), 1);
        assert_eq!(panel.history[1]["result"]["entities"][0]["id"], 42);
    }
}
