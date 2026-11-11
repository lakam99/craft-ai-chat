//! A reusable dock body. Applications own tools, active views, preferences and document identity.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

mod ui;
pub use craft_ai_chat_core as core;
use craft_ai_chat_core::protocol;
pub use craft_ai_chat_core::{HostInfo, Preferences, ToolCall};
use craft_ai_chat_native::{
    Wake,
    credentials::{Action, Vault},
};
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
    fn credential_profile(&self) -> Option<CredentialProfile> {
        None
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
    call: ToolCall,
    reply: mpsc::Sender<Value>,
    deadline: Instant,
    tool: Option<T>,
}
enum Event {
    Tool(ToolCall, mpsc::Sender<Value>),
    Done(Result<protocol::Completed, String>),
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
    transcript: Vec<(String, String)>,
    history: Vec<Value>,
    history_model: String,
    history_instance: Option<Arc<()>>,
    pending: Option<Pending<T>>,
    preferences: Preferences,
    vault: Vault,
    remove_saved_key: bool,
    host_id: String,
    profile: Option<CredentialProfile>,
}
impl<T> Default for Panel<T> {
    fn default() -> Self {
        Self {
            key: String::new(),
            prompt: String::new(),
            message: String::new(),
            transcript: Vec::new(),
            history: Vec::new(),
            history_model: String::new(),
            history_instance: None,
            pending: None,
            preferences: Preferences::default(),
            vault: Vault::default(),
            remove_saved_key: false,
            host_id: String::new(),
            profile: None,
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
        &self.transcript
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    pub fn fail(&mut self, error: &str) {
        self.pending = None;
        self.history.clear();
        self.message = error.into();
        self.record("Notice", error.into());
    }
    fn record(&mut self, role: &str, text: String) {
        if self.transcript.len() >= 128 {
            self.transcript.remove(0);
        }
        self.transcript
            .push((role.into(), text.chars().take(32_768).collect()));
    }
    fn sync<H: Host<Tool = T>>(&mut self, host: &H, ctx: &egui::Context) {
        let profile = host.credential_profile();
        if !self.host_id.is_empty() && (self.host_id != host.info().id || self.profile != profile) {
            self.stop();
            self.key.clear();
            self.history.clear();
            self.transcript.clear();
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
    }
    fn initialize_vault(&mut self, ctx: &egui::Context) {
        let path = self.profile.as_ref().map(|p| p.preferences_file.as_path());
        let service = self.profile.as_ref().map_or("", |p| p.service.as_str());
        self.vault
            .initialize(path, service, self.preferences.remember_key, wake(ctx));
    }
    pub fn stop(&mut self) {
        if let Some(pending) = &self.pending {
            pending.cancel.store(true, Ordering::Release);
            self.message="Stopping. No further tools will run; an in-flight API request may take up to 120 seconds to finish.".into();
        }
    }
    pub fn status<H: Host<Tool = T>>(&self, host: &H) -> Value {
        json!({"open":host.chat_is_open(),"busy":self.busy(),"hasKey":self.has_key(),"savedKey":self.vault.saved(),"credentialBusy":self.vault.busy(),"credentialMessage":self.vault.message,"message":self.message,"transcript":self.transcript})
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
                | "ai.send"
                | "ai.stop"
                | "ai.clear"
                | "ai.status"
                | "ai.saveKey"
                | "ai.loadKey"
                | "ai.forgetKey"
        ) {
            return None;
        }
        self.sync(host, ctx);
        Some(match id {
            "ai.settings" => host.open_preferences().map(|()| Value::Null),
            "ai.send" => self.send(host, ctx),
            "ai.stop" => {
                self.stop();
                Ok(Value::Null)
            }
            "ai.clear" if !self.busy() => {
                self.history.clear();
                self.transcript.clear();
                self.message.clear();
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
        if self.history_model != model
            || self
                .history_instance
                .as_ref()
                .is_some_and(|old| !Arc::ptr_eq(old, &instance))
        {
            self.history.clear();
            if !self.transcript.is_empty() {
                self.record(
                    "Notice",
                    "New model or document: starting a new conversation context.".into(),
                );
            }
        }
        let info = host.info().clone();
        let attach = self.preferences.attach_frames;
        let mut history = self.history.clone();
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
                    wake(&context)();let deadline=Instant::now()+Duration::from_secs(35);
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
                history.push(json!({"role":"user","content":content}));
                protocol::run(&info,&worker_model,history,&cancelled,request,call)
            };
            let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or_else(|_|Err("The AI worker failed. Your document is still open.".into()));
            let _=sender.send(Event::Done(result));wake(&context)();
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
        ctx.request_repaint();
        Ok(json!({"pending":true}))
    }
    pub fn poll<H: Host<Tool = T>>(&mut self, host: &mut H, ctx: &egui::Context) {
        self.sync(host, ctx);
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
            Ok(Event::Done(result)) => {
                match protocol::check_cancel(&pending.cancel).and(result) {
                    Ok(done) => {
                        self.history = done.history;
                        self.record("Assistant", done.text);
                        self.message = "Ready".into();
                    }
                    Err(error) => {
                        self.history.clear();
                        self.record("Notice", error.clone());
                        self.message = error;
                    }
                }
                return;
            }
            Ok(Event::Tool(call, reply)) => {
                pending.tool = Some(ToolJob {
                    call,
                    reply,
                    deadline: Instant::now() + Duration::from_secs(30),
                    tool: None,
                })
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.fail("The AI worker stopped. Your document is still open.");
                return;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if let Some(mut job) = pending.tool.take() {
            let result = if pending.cancel.load(Ordering::Acquire) {
                Some(Value::String(
                    json!({"ok":false,"error":"AI stopped or a different document was opened"})
                        .to_string(),
                ))
            } else if Instant::now() > job.deadline {
                pending.cancel.store(true, Ordering::Release);
                Some(Value::String("Host tool timed out".into()))
            } else {
                let mut work = || -> Result<Option<Value>, String> {
                    host.info().validate_call(&job.call)?;
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
                let detail = if result.is_array() {
                    "view attached".into()
                } else {
                    result
                        .as_str()
                        .unwrap_or("completed")
                        .chars()
                        .take(1024)
                        .collect::<String>()
                };
                let arguments = job
                    .call
                    .arguments
                    .to_string()
                    .chars()
                    .take(1024)
                    .collect::<String>();
                self.record("Tool", format!("{} {arguments}\n{detail}", job.call.name));
                let _ = job.reply.send(result);
            } else {
                pending.tool = Some(job);
            }
        }
        self.pending = Some(pending);
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}
