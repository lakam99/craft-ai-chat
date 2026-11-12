//! A reusable dock body. Applications own tools, active views, preferences and document identity.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

mod archive;
mod batch;
mod conversation;
mod guidance;
mod ui;
use conversation::Conversation;
pub use conversation::{Activity, ActivityState, command_id, tool_label};
pub use craft_ai_chat_core as core;
use craft_ai_chat_core::protocol;
pub use craft_ai_chat_core::{HostInfo, Preferences, ToolCall};
use craft_ai_chat_native::{
    Wake,
    credentials::{Action, Vault},
};
pub use guidance::{HighlightTarget, Observation};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// The path identifies an editor preference profile, not a document or project.
#[derive(Clone, PartialEq, Eq)]
pub struct CredentialProfile {
    pub preferences_file: std::path::PathBuf,
    pub service: String,
}

/// Only the UI thread calls the adapter. Its pending tool type never crosses a worker boundary.
pub trait Host {
    type Tool;
    fn info(&self) -> &HostInfo;
    fn preferences(&self) -> Preferences;
    fn instance(&self) -> Arc<()>;
    /// Stable project path/identity for local chat archives; never a credential.
    fn document_key(&self) -> Option<String> {
        None
    }
    fn credential_profile(&self) -> Option<CredentialProfile> {
        None
    }
    fn tool_label(&self, call: &ToolCall) -> String {
        tool_label(call)
    }
    fn highlight_target(&self, _id: &str) -> Option<HighlightTarget> {
        None
    }
    /// Current-pass widget events only, never synthesized clicks or cached coordinates.
    fn guide_observation(&self, _id: &str, _ctx: &egui::Context) -> Option<Observation> {
        None
    }
    /// Small, read-only document counters/state. Exclude credentials and private UI drafts.
    fn guide_state(&self) -> Value {
        Value::Null
    }
    fn start_tool(&mut self, call: &ToolCall) -> Result<Self::Tool, String>;
    fn poll_tool(&mut self, tool: &mut Self::Tool) -> Result<Option<Value>, String>;
    fn set_preference(&mut self, key: &str, value: Value) -> Result<(), String>;
    fn open_preferences(&mut self) -> Result<(), String>;
    fn chat_is_open(&self) -> bool {
        true
    }
    fn widget(&mut self, id: &str, response: &egui::Response, label: &str);
    fn area(&mut self, id: &str, rect: egui::Rect, label: &str);
}

struct ToolJob<T> {
    activity_id: u64,
    call: ToolCall,
    reply: mpsc::Sender<Value>,
    deadline: Instant,
    tool: Option<T>,
    started: bool,
    batch: Option<batch::Batch<T>>,
}
enum Event {
    Tool(ToolCall, mpsc::Sender<Value>),
    Checkpoint(Vec<Value>),
    Done(Result<String, String>, Vec<Value>),
}
struct Pending<T> {
    events: mpsc::Receiver<Event>,
    cancel: Arc<AtomicBool>,
    instance: Arc<()>,
    tool: Option<ToolJob<T>>,
}
impl<T> Drop for Pending<T> {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

/// Session-only state. Intentionally neither serializable nor Debug: keys and chats stay private.
pub struct Panel<T> {
    key: String,
    prompt: String,
    message: String,
    chat: Conversation,
    markdown_cache: egui_commonmark::CommonMarkCache,
    markdown_oldest: Option<u64>,
    connection_open: bool,
    history: Vec<Value>,
    history_model: String,
    history_instance: Option<Arc<()>>,
    pending: Option<Pending<T>>,
    preferences: Preferences,
    vault: Vault,
    remove_saved_key: bool,
    host_id: String,
    profile: Option<CredentialProfile>,
    guidance: guidance::Guidance,
    archive: archive::Archive,
    batch_progress: Option<Value>,
}
impl<T> Default for Panel<T> {
    fn default() -> Self {
        Self {
            key: String::new(),
            prompt: String::new(),
            message: String::new(),
            chat: Conversation::default(),
            markdown_cache: egui_commonmark::CommonMarkCache::default(),
            markdown_oldest: None,
            connection_open: false,
            history: Vec::new(),
            history_model: String::new(),
            history_instance: None,
            pending: None,
            preferences: Preferences::default(),
            vault: Vault::default(),
            remove_saved_key: false,
            host_id: String::new(),
            profile: None,
            guidance: guidance::Guidance::default(),
            archive: archive::Archive::default(),
            batch_progress: None,
        }
    }
}

fn wake(ctx: &egui::Context) -> Wake {
    let ctx = ctx.clone();
    Arc::new(move || {
        ctx.request_repaint();
        ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
    })
}
fn clear_key_undo(ctx: &egui::Context) {
    for field in ["ai.key.field", "ai.key.preferences"] {
        egui::text_edit::TextEditState::default().store(ctx, egui::Id::unique(field));
    }
}
impl<T> Panel<T> {
    pub fn prompt_mut(&mut self) -> &mut String {
        &mut self.prompt
    }
    pub fn set_session_key(&mut self, key: String) -> Result<(), String> {
        if !key.is_empty() {
            craft_ai_chat_core::validate_key(&key)?;
        }
        self.key = key;
        Ok(())
    }
    pub fn has_key(&self) -> bool {
        !self.key.is_empty()
    }
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }
    pub fn context_items(&self) -> usize {
        self.history.len()
    }
    pub fn transcript(&self) -> &[(String, String)] {
        &self.chat.transcript
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    pub fn fail(&mut self, error: &str) {
        self.guidance.clear();
        self.pending = None;
        core::history::close_pending(&mut self.history, error);
        self.chat.interrupt(ActivityState::Failed, error);
        self.message = error.into();
        self.record("Notice", error.into());
        self.persist();
    }
    fn record(&mut self, role: &str, text: String) {
        self.chat.message(role, text);
    }
    fn sync<H: Host<Tool = T>>(&mut self, host: &H, ctx: &egui::Context) {
        let profile = host.credential_profile();
        if !self.host_id.is_empty() && (self.host_id != host.info().id || self.profile != profile) {
            self.persist();
            self.archive = archive::Archive::default();
            self.stop();
            self.key.clear();
            self.history.clear();
            self.chat.clear();
            self.markdown_cache = egui_commonmark::CommonMarkCache::default();
            self.vault = Vault::default();
            clear_key_undo(ctx);
            self.remove_saved_key = false;
            self.history_instance = None;
        }
        self.host_id = host.info().id.clone();
        self.profile = profile;
        let prefs = host.preferences();
        if self.preferences.remember_key && !prefs.remember_key && self.vault.configured() {
            self.remove_saved_key = true;
        }
        self.preferences = prefs;
        self.sync_archive(host, ctx);
    }
    fn initialize_vault(&mut self, ctx: &egui::Context) {
        let path = self.profile.as_ref().map(|p| p.preferences_file.as_path());
        let service = self.profile.as_ref().map_or("", |p| p.service.as_str());
        self.vault
            .initialize(path, service, self.preferences.remember_key, wake(ctx));
    }
    pub fn stop(&mut self) {
        self.guidance.clear();
        if let Some(pending) = &self.pending {
            pending.cancel.store(true, Ordering::Release);
            self.message="Stopping. No further tools will run; an in-flight API request may take up to 120 seconds to finish.".into();
        }
    }
    pub fn status<H: Host<Tool = T>>(&self, host: &H) -> Value {
        json!({"open":host.chat_is_open(),"busy":self.busy(),"hasKey":self.has_key(),"savedKey":self.vault.saved(),"credentialBusy":self.vault.busy(),"credentialMessage":self.vault.message,"message":self.message,"transcript":self.chat.transcript,"highlights":self.guidance.status(),"walkthrough":self.guidance.walkthrough_status(),"historyMessage":self.archive.message,"contextItems":self.history.len()})
    }
    pub fn command<H: Host<Tool = T>>(
        &mut self,
        host: &mut H,
        ctx: &egui::Context,
        id: &str,
    ) -> Option<Result<Value, String>> {
        if !matches!(
            id,
            "ai.settings"
                | "ai.connection"
                | "ai.send"
                | "ai.stop"
                | "ai.clear"
                | "ai.status"
                | "ai.clearHighlights"
                | "ai.saveKey"
                | "ai.loadKey"
                | "ai.forgetKey"
        ) {
            return None;
        }
        self.sync(host, ctx);
        Some(match id {
            "ai.connection" => {
                self.connection_open = !self.connection_open;
                Ok(Value::Null)
            }
            "ai.settings" => host.open_preferences().map(|()| Value::Null),
            "ai.send" => self.send(host, ctx),
            "ai.clearHighlights" => {
                self.dismiss_guidance();
                Ok(Value::Null)
            }
            "ai.stop" => {
                self.stop();
                Ok(Value::Null)
            }
            "ai.clear" if !self.busy() => {
                self.guidance.clear();
                self.history.clear();
                self.chat.clear();
                self.markdown_cache = egui_commonmark::CommonMarkCache::default();
                self.message.clear();
                self.persist();
                Ok(Value::Null)
            }
            "ai.clear" => Err("Stop the active AI request before clearing the chat".into()),
            "ai.status" => Ok(self.status(host)),
            "ai.saveKey" => {
                self.initialize_vault(ctx);
                self.vault
                    .start(Action::Save(self.key.clone()), wake(ctx))
                    .map(|()| Value::Null)
            }
            "ai.loadKey" => {
                self.initialize_vault(ctx);
                self.vault
                    .start(Action::Load, wake(ctx))
                    .map(|()| Value::Null)
            }
            "ai.forgetKey" if self.vault.busy() => {
                Err("Wait for the OS credential operation to finish".into())
            }
            "ai.forgetKey" => {
                self.key.clear();
                clear_key_undo(ctx);
                if self.vault.configured() {
                    self.vault
                        .start(Action::Delete, wake(ctx))
                        .map(|()| Value::Null)
                } else {
                    Ok(Value::Null)
                }
            }
            _ => Err("Unsupported AI command".into()),
        })
    }
    fn send<H: Host<Tool = T>>(
        &mut self,
        host: &mut H,
        ctx: &egui::Context,
    ) -> Result<Value, String> {
        if self.vault.busy() {
            return Err("Wait for the OS credential operation to finish".into());
        }
        let request = craft_ai_chat_native::http(self.key.trim().to_string())?;
        let result = self.send_with(host, ctx, request)?;
        if self.preferences.remember_key && self.profile.is_some() && !self.vault.matches(&self.key)
        {
            self.initialize_vault(ctx);
            if let Err(error) = self
                .vault
                .start(Action::Save(self.key.trim().to_string()), wake(ctx))
            {
                self.vault.message = error;
            }
        }
        Ok(result)
    }
    /// Inject a transport for offline tests or an application-owned compatible transport.
    /// The production Send command always uses the fixed OpenAI HTTPS endpoint.
    pub fn send_with<H: Host<Tool = T>>(
        &mut self,
        host: &mut H,
        ctx: &egui::Context,
        request: impl FnMut(&Value) -> Result<Value, String> + Send + 'static,
    ) -> Result<Value, String> {
        self.sync(host, ctx);
        if self.archive.loading() {
            return Err("Loading saved chat history; try again in a moment".into());
        }
        if self.busy() {
            return Err("An AI request is already running".into());
        }
        host.info().validate()?;
        let prompt = self.prompt.trim().to_string();
        if prompt.is_empty() || prompt.len() > 32_768 {
            return Err("Enter a message of 1–32768 bytes".into());
        }
        let model = self.preferences.model.trim().to_string();
        if !Preferences::valid_model(&model) {
            return Err("Enter a valid OpenAI model ID".into());
        }
        let instance = host.instance();
        if self
            .history_instance
            .as_ref()
            .is_some_and(|old| !Arc::ptr_eq(old, &instance))
        {
            self.history.clear();
            if !self.chat.transcript.is_empty() {
                self.record(
                    "Notice",
                    "New document: starting a new conversation context.".into(),
                );
            }
        }
        let info = host.info().clone();
        self.guidance.begin_turn();
        self.batch_progress = None;
        let attach = self.preferences.attach_frames;
        let mut history = self.history.clone();
        core::history::omit_media(&mut history);
        core::history::close_pending(&mut history, "The previous turn was interrupted");
        // Compaction can fail while offline. Bound retained recovery data before
        // accepting another message, without discarding the existing task or draft.
        if history.len() >= 512
            || core::bounds::serialized_size(&history, core::MAX_BODY - 64 * 1024).is_err()
        {
            return Err("Chat recovery context is full. Your existing history and draft are preserved. Copy the instructions you need, then start a new chat to continue.".into());
        }
        if self.history_model != model {
            core::history::remove_model_state(&mut history);
        }
        history.push(json!({"role":"user","content":[{"type":"input_text","text":prompt}]}));
        self.history = history.clone();
        let (sender, events) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = cancel.clone();
        let context = ctx.clone();
        let worker_prompt = prompt.clone();
        let worker_model = model.clone();
        std::thread::Builder::new().name("craft-ai-chat".into()).spawn(move||{
            let work=|| {
                let call=|name:&str,args:&Value|->Result<Value,String> {
                    protocol::check_cancel(&cancelled)?;
                    let call=ToolCall::new(name,args.clone());info.validate_call(&call)?;
                    let (reply,receive)=mpsc::channel();sender.send(Event::Tool(call,reply)).map_err(|_|"The host closed".to_string())?;
                    // A guide waits for a person, not a backend. Keep ordinary tool deadlines short.
                    wake(&context)();let deadline=Instant::now()+Duration::from_secs(if name=="guide_ui" { 1805 } else if name=="batch_commands" {305} else { 35 });
                    loop {
                        protocol::check_cancel(&cancelled)?;
                        match receive.recv_timeout(Duration::from_millis(100)) {
                            Ok(value)=>{core::bounds::value_size(&value,core::MAX_REPLY)?;return Ok(value);},
                            Err(mpsc::RecvTimeoutError::Disconnected)=>return Err("The host tool stopped".into()),
                            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now()>=deadline=>{cancelled.store(true,Ordering::Release);return Err("The host did not respond in time; no further tools will run".into());},
                            Err(mpsc::RecvTimeoutError::Timeout)=>context.request_repaint(),
                        }
                    }
                };
                let mut content=vec![json!({"type":"input_text","text":worker_prompt})];
                if attach {for input in &info.automatic_inputs {let result=call(&input.name,&input.arguments)?;if let Some(parts)=result.as_array(){content.extend(parts.iter().cloned());core::bounds::serialized_size(&content,core::MAX_BODY)?;}}}
                if let Some(last)=history.last_mut(){last["content"]=json!(content);}
                protocol::run_with_history(&info,&worker_model,&mut history,&cancelled,request,call,|items|{
                    sender.send(Event::Checkpoint(items.to_vec())).map_err(|_|"The host closed".to_string())?;wake(&context)();Ok(())
                })
            };
            let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or_else(|_|Err("The AI worker failed. Your document is still open.".into()));
            if let Err(error)=&result {core::history::close_pending(&mut history,error);}
            let _=sender.send(Event::Done(result,history));wake(&context)();
        }).map_err(|_|"Cannot start the AI worker".to_string())?;
        self.pending = Some(Pending {
            events,
            cancel,
            instance: instance.clone(),
            tool: None,
        });
        self.history_model = model;
        self.history_instance = Some(instance);
        self.prompt.clear();
        self.message = "Working…".into();
        self.record("You", prompt);
        self.persist();
        ctx.request_repaint();
        Ok(json!({"pending":true}))
    }
    pub fn poll<H: Host<Tool = T>>(&mut self, host: &mut H, ctx: &egui::Context) {
        self.sync(host, ctx);
        self.guidance.sync(host);
        self.vault.poll(&mut self.key);
        if self.remove_saved_key && !self.vault.busy() {
            self.remove_saved_key = false;
            if let Err(e) = self.vault.start(Action::Delete, wake(ctx)) {
                self.vault.message = e;
            }
        }
        if self.vault.busy() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.poll_inner(host, ctx)))
            .is_err()
        {
            self.fail("The AI tool failed. Your document remains open; review the last action before continuing.");
        }
    }
    fn poll_inner<H: Host<Tool = T>>(&mut self, host: &mut H, ctx: &egui::Context) {
        let Some(mut pending) = self.pending.take() else {
            return;
        };
        if !Arc::ptr_eq(&pending.instance, &host.instance()) {
            pending.cancel.store(true, Ordering::Release);
        }
        match pending.events.try_recv() {
            Ok(Event::Checkpoint(history)) => {
                if Arc::ptr_eq(&pending.instance, &host.instance()) {
                    self.history = history;
                    self.persist();
                }
            }
            Ok(Event::Done(result, history)) => {
                if Arc::ptr_eq(&pending.instance, &host.instance()) {
                    self.history = history;
                    if result.is_err()
                        && let Some(progress) = self.batch_progress.take()
                    {
                        self.history.push(progress);
                    }
                } else if self
                    .history_instance
                    .as_ref()
                    .is_some_and(|i| Arc::ptr_eq(i, &pending.instance))
                {
                    self.history.clear();
                    self.history_instance = None;
                }
                self.batch_progress = None;
                self.guidance.finish_walkthrough();
                match protocol::check_cancel(&pending.cancel).and(result) {
                    Ok(done) => {
                        self.record("Assistant", done);
                        self.message = "Ready".into();
                    }
                    Err(error) => {
                        self.chat.interrupt(
                            if pending.cancel.load(Ordering::Acquire) {
                                ActivityState::Stopped
                            } else {
                                ActivityState::Failed
                            },
                            &error,
                        );
                        self.record("Notice", error.clone());
                        self.message = error;
                    }
                }
                self.persist();
                return;
            }
            Ok(Event::Tool(call, reply)) => {
                let activity_id = self.chat.begin(host.tool_label(&call));
                pending.tool = Some(ToolJob {
                    activity_id,
                    deadline: Instant::now()
                        + Duration::from_secs(if call.name == "guide_ui" {
                            1800
                        } else if call.name == "batch_commands" {
                            300
                        } else {
                            30
                        }),
                    call,
                    reply,
                    tool: None,
                    started: false,
                    batch: None,
                })
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.fail("The AI worker stopped. Your document is still open.");
                return;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if let Some(mut job) = pending.tool.take() {
            let timed_out =
                Instant::now() > job.deadline && !pending.cancel.load(Ordering::Acquire);
            let result = if pending.cancel.load(Ordering::Acquire) {
                Some(Value::String(
                    json!({"ok":false,"error":"AI stopped or a different document was opened"})
                        .to_string(),
                ))
            } else if timed_out {
                pending.cancel.store(true, Ordering::Release);
                Some(Value::String(
                    json!({"ok":false,"error":"Host tool timed out"}).to_string(),
                ))
            } else {
                let mut work = || -> Result<Option<Value>, String> {
                    host.info().validate_call(&job.call)?;
                    if job.call.name == "batch_commands" {
                        if job.batch.is_none() {
                            job.batch = Some(batch::Batch::new(host.info(), &job.call.arguments)?);
                        }
                        let batch = job.batch.as_mut().ok_or("Cannot start command batch")?;
                        let result = batch.poll(host)?;
                        if batch.completed > 0 {
                            let commands = job.call.arguments["commands"]
                                .as_array()
                                .map(|a| {
                                    a.iter().take(batch.completed).cloned().collect::<Vec<_>>()
                                })
                                .unwrap_or_default();
                            self.batch_progress = Some(
                                json!({"role":"user","content":format!("Observed batch progress (historical data, not a new request). These commands completed; do not replay them. Inspect live state before continuing: {}",json!({"completed":batch.completed,"total":batch.total(),"completedCommands":commands}))}),
                            );
                        }
                        self.chat.finish(
                            job.activity_id,
                            ActivityState::Running,
                            format!(
                                "{} of {} commands completed",
                                batch.completed,
                                batch.total()
                            ),
                        );
                        self.persist();
                        return Ok(result);
                    }
                    if job.call.name == "highlight_ui" {
                        return self.guidance.apply(host, &job.call.arguments).map(Some);
                    }
                    if job.call.name == "guide_state" {
                        if !job
                            .call
                            .arguments
                            .as_object()
                            .is_some_and(|args| args.is_empty())
                        {
                            return Err("guide_state takes no parameters".into());
                        }
                        return guidance::state(host).map(Some);
                    }
                    if job.call.name == "guide_ui" {
                        if !job.started {
                            let instruction =
                                self.guidance.start_step(host, &job.call.arguments)?;
                            self.record("Assistant", instruction);
                            self.message =
                                "Waiting for you to complete the highlighted step".into();
                            job.started = true;
                        }
                        let result = self.guidance.step_result(host)?;
                        if result.is_some() {
                            self.message = "Checking the next step…".into();
                        }
                        return Ok(result);
                    }
                    if job.tool.is_none() {
                        job.tool = Some(host.start_tool(&job.call)?);
                    }
                    let tool = job.tool.as_mut().ok_or("Cannot start the host tool")?;
                    host.poll_tool(tool)
                };
                match work() {
                    Ok(result) => result,
                    Err(e) => Some(Value::String(json!({"ok":false,"error":e}).to_string())),
                }
            };
            if let Some(result) = result {
                let (state, detail) = if timed_out {
                    (ActivityState::Failed, "Host tool timed out".into())
                } else if pending.cancel.load(Ordering::Acquire) {
                    (ActivityState::Stopped, "No further actions will run".into())
                } else {
                    conversation::outcome(&result)
                };
                self.chat.finish(job.activity_id, state, detail);
                let _ = job.reply.send(result);
            } else {
                pending.tool = Some(job);
            }
        }
        self.pending = Some(pending);
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod ui_tests;
