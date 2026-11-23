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
pub mod client;
mod conversation;
mod goals;
mod guidance;
pub mod service;
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
type Request = Arc<std::sync::Mutex<Box<dyn FnMut(&Value) -> Result<Value, String> + Send>>>;
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
    /// A process bridge resolves current-pass guidance in the editor process.
    fn external_guidance(&self) -> bool {
        false
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
    Checkpoint(Vec<Value>, mpsc::Sender<Result<(), String>>),
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
    goal_mode: bool,
    goal: Option<goals::Shared>,
    queued: Vec<String>,
    resume_queued: bool,
    transport: Option<Request>,
    continuing: bool,
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
            goal_mode: false,
            goal: None,
            queued: vec![],
            resume_queued: false,
            transport: None,
            continuing: false,
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
        if self.key != key {
            self.transport = None;
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
            self.goal = None;
            self.goal_mode = false;
            self.queued.clear();
            self.resume_queued = false;
            self.transport = None;
        }
        self.host_id = host.info().id.clone();
        self.profile = profile;
        let prefs = host.preferences();
        if self.preferences.remember_key && !prefs.remember_key && self.vault.configured() {
            self.remove_saved_key = true;
        }
        if self.preferences.remember_history && !prefs.remember_history && self.goal_mode {
            self.stop();
            self.goal_mode = false;
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
        self.resume_queued = false;
        if let Some(goal) = &self.goal
            && let Err(error) =
                goals::lock(goal).pause("Paused by you. Completed edits are retained.")
        {
            self.message = error;
        }
        if let Some(pending) = &self.pending {
            pending.cancel.store(true, Ordering::Release);
            self.message="Pausing after the current edit. An in-flight API request may take up to 120 seconds to return.".into();
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
                | "ai.history"
                | "ai.goal"
                | "ai.goal.resume"
                | "ai.goal.review"
        ) {
            if let Some(chat) = id.strip_prefix("ai.history.open.") {
                return Some(
                    self.switch_chat(host, ctx, Some(chat))
                        .map(|()| Value::Null),
                );
            }
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
            "ai.history" => {
                self.archive.browsing = !self.archive.browsing;
                Ok(Value::Null)
            }
            "ai.goal" if !self.busy() => {
                if !self.goal_mode && (!self.preferences.remember_history || self.profile.is_none())
                {
                    Err("Enable local chat history in editor preferences before using continuous goals".into())
                } else {
                    self.goal_mode = !self.goal_mode;
                    self.persist();
                    Ok(Value::Null)
                }
            }
            "ai.goal.resume" => self.resume_goal(host, ctx),
            "ai.goal.review" if !self.busy() => {
                let goal = self
                    .goal
                    .as_ref()
                    .ok_or_else(|| "There is no saved goal".to_string());
                goal.and_then(|goal|{let mut session=goals::lock(goal);
                    if let Some(step)=session.goal.pending.clone(){
                        // Acknowledgment is a tombstone, never a success receipt or a replay.
                        let result=json!({"ok":false,"uncertain":true,"reviewedByUser":true,"doNotReplay":true});
                        let receipt=craft_ai_chat_native::journal::Receipt{goal:session.goal.id.clone(),sequence:session.goal.completed.saturating_add(1),id:step.id.clone(),call:step.call,result:result.clone()};
                        session.store_record(&receipt)?;session.goal.after(&step.id,&result)?;
                    }
                    session.goal.needs_review=false;session.goal.pause("Uncertain outcome acknowledged. Resume will inspect current state before further edits.",false);session.save()?;Ok(Value::Null)})
            }
            "ai.clearHighlights" => {
                self.dismiss_guidance();
                Ok(Value::Null)
            }
            "ai.stop" => {
                self.stop();
                Ok(Value::Null)
            }
            "ai.clear" if !self.busy() => self.switch_chat(host, ctx, None).map(|()| Value::Null),
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
                self.transport = None;
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
    fn resume_goal<H: Host<Tool = T>>(
        &mut self,
        host: &mut H,
        ctx: &egui::Context,
    ) -> Result<Value, String> {
        if self.busy() {
            return Err("Wait for the current action to finish pausing".into());
        }
        let goal = self.goal.clone().ok_or("There is no saved goal")?;
        {
            let mut session = goals::lock(&goal);
            session.goal.resume(
                &host
                    .document_key()
                    .unwrap_or_else(|| "unsaved-session".into()),
            )?;
            session.save()?;
        }
        self.goal_mode = true;
        self.continuing = true;
        let draft=std::mem::replace(&mut self.prompt,"Resume the saved goal. Inspect current state and saved receipts before any remaining edits.".into());
        let result = if let Some(request) = self.transport.clone() {
            self.send_using(host, ctx, request)
        } else {
            self.send(host, ctx)
        };
        self.continuing = false;
        self.prompt = draft;
        if result.is_err() {
            let _ =
                goals::lock(&goal).pause("Could not resume. Fix the reported error, then Resume.");
        }
        result
    }
    /// Inject a transport for offline tests or an application-owned compatible transport.
    /// The production Send command always uses the fixed OpenAI HTTPS endpoint.
    pub fn send_with<H: Host<Tool = T>>(
        &mut self,
        host: &mut H,
        ctx: &egui::Context,
        request: impl FnMut(&Value) -> Result<Value, String> + Send + 'static,
    ) -> Result<Value, String> {
        let request: Request = Arc::new(std::sync::Mutex::new(Box::new(request)));
        self.send_using(host, ctx, request)
    }
    fn send_using<H: Host<Tool = T>>(
        &mut self,
        host: &mut H,
        ctx: &egui::Context,
        request: Request,
    ) -> Result<Value, String> {
        self.sync(host, ctx);
        if self.archive.blocked {
            return Err("Saved chat could not be loaded or saved. Original files were kept. Use New chat to continue separately, or resolve the history storage error.".into());
        }
        if self.archive.loading() {
            return Err("Loading saved chat history; try again in a moment".into());
        }
        host.info().validate()?;
        let mut prompt = self.prompt.trim().to_string();
        if prompt.is_empty() || prompt.len() > 32_768 {
            return Err("Enter a message of 1–32768 bytes".into());
        }
        if !self.key.trim().is_empty() {
            prompt = prompt.replace(self.key.trim(), "[API key omitted]");
        }
        if self.busy() {
            if self.queued.len() >= 8
                || self
                    .queued
                    .iter()
                    .map(String::len)
                    .sum::<usize>()
                    .saturating_add(prompt.len())
                    > 32768
            {
                return Err("Eight interruptions or 32 KiB are queued. Wait for the current edit to finish; your draft is retained.".into());
            }
            if self.goal_mode
                && let Some(goal) = &self.goal
            {
                goals::lock(goal).steer(prompt.clone())?;
            }
            self.queued.push(prompt.clone());
            self.resume_queued = true;
            if let Some(pending) = &self.pending {
                pending.cancel.store(true, Ordering::Release);
            }
            self.record("You", prompt);
            self.prompt.clear();
            self.message =
                "Correction queued. Finishing the current edit before continuing…".into();
            self.persist();
            return Ok(json!({"pending":true,"interrupt":true}));
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
        if self.goal_mode {
            if !self.preferences.remember_history {
                return Err("Continuous goals require local chat history".into());
            }
            let document = host
                .document_key()
                .unwrap_or_else(|| "unsaved-session".into());
            let profile = self
                .profile
                .as_ref()
                .ok_or("Continuous goals require an editor preferences profile")?;
            if self
                .goal
                .as_ref()
                .is_none_or(|goal| goals::lock(goal).goal.phase == core::goal::Phase::Complete)
            {
                let goal = core::goal::Goal::new(
                    craft_ai_chat_native::library::new_id(),
                    document.clone(),
                    prompt.clone(),
                )?;
                self.goal = Some(goals::Session::open(profile, &self.host_id, goal, false)?);
            } else if let Some(goal) = &self.goal {
                let mut session = goals::lock(goal);
                if !self.continuing {
                    session.steer(prompt.clone())?;
                }
                session.goal.resume(&document)?;
                session.save()?;
            }
        }
        let goal = if self.goal_mode {
            self.goal.clone()
        } else {
            None
        };
        let info = if goal.is_some() {
            goals::info(host.info().clone())
        } else {
            host.info().clone()
        };
        info.validate()?;
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
        for queued in &self.queued {
            history.push(json!({"role":"user","content":queued}));
        }
        history.push(json!({"role":"user","content":[{"type":"input_text","text":prompt}]}));
        self.history = history.clone();
        let (sender, events) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = cancel.clone();
        let context = ctx.clone();
        let worker_prompt = prompt.clone();
        let worker_model = model.clone();
        let worker_key = self.key.trim().to_owned();
        let durable = self.archive.store.is_some();
        self.transport = Some(request.clone());
        std::thread::Builder::new().name("craft-ai-chat".into()).spawn(move||{
            let work=|| {
                let call=|name:&str,args:&Value|->Result<Value,String> {
                    protocol::check_cancel(&cancelled)?;
                    let call=ToolCall::new(name,args.clone());info.validate_call(&call)?;
                    let (reply,receive)=mpsc::channel();sender.send(Event::Tool(call,reply)).map_err(|_|"The host closed".to_string())?;
                    // A guide waits for a person, not a backend. Keep ordinary tool deadlines short.
                    wake(&context)();let deadline=Instant::now()+Duration::from_secs(if name=="guide_ui" { 1805 } else if name=="batch_commands" {305} else { 35 });
                    loop {
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
                let checkpoint=|items:&[Value]|{
                    let (reply,receive)=mpsc::channel();
                    sender.send(Event::Checkpoint(items.to_vec(),reply)).map_err(|_|"The host closed".to_string())?;wake(&context)();
                    receive.recv_timeout(Duration::from_secs(15)).map_err(|_|"Chat checkpoint was not confirmed; no further edits will run".to_string())?
                };
                if durable {checkpoint(&history)?;}
                if let Some(goal)=&goal {
                    loop {
                        protocol::check_cancel(&cancelled)?;
                        let start=goals::lock(goal).goal.completed;
                        let mut observer=goals::Observer{session:goal.clone(),checkpoint,end:protocol::SegmentEnd::Reply,segment_start:start,key:worker_key.clone()};
                        let result=protocol::run_observed(&info,&worker_model,&mut history,&cancelled,
                            |body|goals::request(goal,body,&request),
                            |name,args|if name.starts_with("goal_"){goals::local_tool(goal,name,args,&cancelled,&request,&worker_model,||call("guide_state",&json!({})))}else{call(name,args)},
                            &mut observer);
                        if result.is_err(){let mut session=goals::lock(goal);if session.goal.phase==core::goal::Phase::Active{session.goal.pause("The request stopped. Review the reported error before resuming.",false);session.goal.needs_review|=session.goal.pending.is_some();session.save()?;}}
                        if result.is_ok() && observer.end==protocol::SegmentEnd::ToolBudget && goals::lock(goal).goal.phase==core::goal::Phase::Active {
                            history.push(json!({"role":"user","content":"Continue the saved goal from actual receipts and live state. Do not replay completed edits."}));
                        }else{return result;}
                    }
                }else{
                    protocol::run_with_history(&info,&worker_model,&mut history,&cancelled,
                        |body|request.lock().unwrap_or_else(std::sync::PoisonError::into_inner)(body),call,checkpoint)
                }
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
        if !self.continuing {
            self.record("You", prompt);
        }
        self.queued.clear();
        self.resume_queued = false;
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
            Ok(Event::Checkpoint(history, reply)) => {
                if Arc::ptr_eq(&pending.instance, &host.instance()) {
                    self.history = history;
                    self.persist();
                    if let Some(store) = &mut self.archive.store {
                        if self.archive.saved {
                            store.confirm(reply);
                        } else {
                            let _ = reply.send(Err(
                                "Chat could not be checkpointed; no further edits will run".into(),
                            ));
                        }
                    } else {
                        let _ = reply.send(Ok(()));
                    }
                } else {
                    let _ = reply.send(Err("The active project changed".into()));
                }
            }
            Ok(Event::Done(result, history)) => {
                let same = Arc::ptr_eq(&pending.instance, &host.instance());
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
                if same && self.resume_queued && !self.queued.is_empty() {
                    let queued = self.queued.clone();
                    let request = self.transport.clone();
                    drop(pending);
                    if let Some(request) = request {
                        let draft = std::mem::replace(&mut self.prompt, queued.join("\n\n"));
                        self.queued.clear();
                        self.continuing = true;
                        let result = self.send_using(host, ctx, request);
                        self.continuing = false;
                        self.prompt = draft;
                        if let Err(error) = result {
                            self.queued = queued;
                            self.message = error;
                            self.resume_queued = false;
                            self.persist();
                        }
                    }
                } else {
                    self.resume_queued = false;
                }
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
            let stopping = pending.cancel.load(Ordering::Acquire);
            let same = Arc::ptr_eq(&pending.instance, &host.instance());
            let active =
                job.tool.is_some() || job.batch.as_ref().is_some_and(|batch| batch.active());
            let timed_out = Instant::now() > job.deadline && (!stopping || active);
            let result = if !same || (stopping && !active) {
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
                        let result = batch.poll_stopping(host, stopping)?;
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
                    if job.call.name == "highlight_ui" && !host.external_guidance() {
                        return self.guidance.apply(host, &job.call.arguments).map(Some);
                    }
                    if job.call.name == "guide_state" && !host.external_guidance() {
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
                    if job.call.name == "guide_ui" && !host.external_guidance() {
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
