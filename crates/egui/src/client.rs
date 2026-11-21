//! Stable native renderer and private-pipe client. No editor authority crosses into the service.
use crate::{Host, Panel as LocalPanel};
use craft_ai_chat_core::runtime::{self, Input, Node, Output, Profile, Snapshot, TextStyle, View};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::BufReader,
    ops::{Deref, DerefMut},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

struct Connection {
    child: Child,
    send: mpsc::SyncSender<Input>,
    receive: mpsc::Receiver<Result<Output, String>>,
    ready: bool,
    started: Instant,
    version: String,
}
impl Drop for Connection {
    fn drop(&mut self) {
        // A stopped/crashed component must never keep a host editor alive.
        let _ = self.send.try_send(Input::Shutdown);
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Connection {
    fn start(path: PathBuf, ctx: &egui::Context) -> Result<Self, String> {
        let mut command = Command::new(path);
        command
            .arg("--stdio-v1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW, keep the docked editor in focus.
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("Cannot start chat component: {e}"))?;
        let stdin = child.stdin.take().ok_or("Chat input pipe unavailable")?;
        let stdout = child.stdout.take().ok_or("Chat output pipe unavailable")?;
        let (send, commands) = mpsc::sync_channel::<Input>(32);
        let (events, receive) = mpsc::sync_channel(8);
        let wake = ctx.clone();
        std::thread::Builder::new()
            .name("chat-component-input".into())
            .spawn(move || {
                let work = || {
                    let mut stdin = stdin;
                    while let Ok(packet) = commands.recv() {
                        if runtime::write(&mut stdin, &packet).is_err() {
                            break;
                        }
                    }
                };
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
            })
            .map_err(|e| format!("Cannot start chat pipe writer: {e}"))?;
        std::thread::Builder::new().name("chat-component-output".into()).spawn(move || {
            let work=|| {
                let mut reader=BufReader::new(stdout);
                loop {
                    let packet=match runtime::read(&mut reader) {Ok(Some(packet))=>Ok(packet),Ok(None)=>Err("Chat component stopped. Reopen the panel to recover saved history.".into()),Err(error)=>Err(error)};
                    let failed=packet.is_err();
                    if events.send(packet).is_err() {break;}
                    wake.request_repaint();
                    if failed {break;}
                }
            };
            let _=std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
        }).map_err(|e|format!("Cannot start chat pipe reader: {e}"))?;
        Ok(Self {
            child,
            send,
            receive,
            ready: false,
            started: Instant::now(),
            version: String::new(),
        })
    }
    fn send(&self, packet: Input) -> Result<(), String> {
        self.send
            .try_send(packet)
            .map_err(|_| "Chat component is busy; try again in a moment".into())
    }
}

/// Drop-in dock body with a source-integrated fallback for offline first launch and tests.
pub struct Panel<T> {
    local: LocalPanel<T>,
    connection: Option<Connection>,
    instance: Option<Arc<()>>,
    epoch: u64,
    snapshot: Vec<u8>,
    view: Option<View>,
    status: Value,
    tools: Vec<(u64, u64, T, Arc<()>)>,
    guide: Option<(u64, u64)>,
    fields: BTreeMap<String, String>,
    markdown: egui_commonmark::CommonMarkCache,
    updater: craft_ai_chat_native::component::Updater,
    failed: bool,
    sync_needed: bool,
    bridge_error: String,
    sent_prompt: String,
    attempted: Vec<PathBuf>,
    requested: bool,
    source_mode: bool,
    next_action: u64,
    sending: Option<(u64, String)>,
}
impl<T> Default for Panel<T> {
    fn default() -> Self {
        Self {
            local: LocalPanel::default(),
            connection: None,
            instance: None,
            epoch: 0,
            snapshot: vec![],
            view: None,
            status: Value::Null,
            tools: vec![],
            guide: None,
            fields: BTreeMap::new(),
            markdown: egui_commonmark::CommonMarkCache::default(),
            updater: Default::default(),
            failed: false,
            sync_needed: false,
            bridge_error: String::new(),
            sent_prompt: String::new(),
            attempted: Vec::new(),
            requested: false,
            source_mode: false,
            next_action: 0,
            sending: None,
        }
    }
}
impl<T> Deref for Panel<T> {
    type Target = LocalPanel<T>;
    fn deref(&self) -> &Self::Target {
        &self.local
    }
}
impl<T> DerefMut for Panel<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.local
    }
}
impl<T> Panel<T> {
    fn receipt(&mut self, receipt: runtime::Receipt) {
        if let Some((serial, draft)) = &self.sending
            && *serial == receipt.serial
        {
            if receipt.accepted && &self.local.prompt == draft {
                self.local.prompt.clear();
            }
            self.sending = None;
        }
    }
    fn choose_component(&mut self, paths: Vec<PathBuf>) -> Option<PathBuf> {
        if self.source_mode {
            return None;
        }
        let candidate = paths.into_iter().find(|p| !self.attempted.contains(p));
        // Installing a component later must not replace a source-integrated conversation.
        if candidate.is_none() && self.attempted.is_empty() {
            self.source_mode = true;
        }
        candidate
    }
    fn snapshot<H: Host<Tool = T>>(&mut self, host: &H) -> Snapshot {
        let instance = host.instance();
        if self
            .instance
            .as_ref()
            .is_none_or(|old| !Arc::ptr_eq(old, &instance))
        {
            self.epoch = self.epoch.saturating_add(1);
            self.instance = Some(instance);
            self.tools.clear();
            self.guide = None;
            self.sending = None;
            self.local.guidance.clear();
            self.view = None;
            self.status = Value::Null;
        }
        Snapshot {
            protocol: runtime::PROTOCOL,
            epoch: self.epoch,
            info: host.info().clone(),
            preferences: host.preferences(),
            document: host.document_key(),
            profile: host.credential_profile().map(|p| Profile {
                preferences_file: p.preferences_file,
                service: p.service,
            }),
            open: host.chat_is_open(),
        }
    }
    fn action<H: Host<Tool = T>>(
        &mut self,
        host: &H,
        id: &str,
        value: Value,
    ) -> Result<u64, String> {
        let snapshot = self.snapshot(host);
        snapshot.validate()?;
        let connection = self
            .connection
            .as_ref()
            .ok_or("Chat component is not running")?;
        if !connection.ready {
            return Err("Chat component is starting; try again in a moment".into());
        }
        self.next_action = self
            .next_action
            .checked_add(1)
            .ok_or("Chat action IDs exhausted")?;
        connection.send(Input::Action {
            serial: self.next_action,
            snapshot,
            id: id.into(),
            value,
        })?;
        Ok(self.next_action)
    }
    pub fn busy(&self) -> bool {
        self.sending.is_some()
            || self.connection.as_ref().map_or_else(
                || self.local.busy(),
                |_| self.status["busy"].as_bool().unwrap_or(true),
            )
    }
    pub fn fail(&mut self, error: &str) {
        if let Some(connection) = &self.connection {
            let _ = connection.send(Input::Failure {
                message: error.into(),
            });
        } else if self.failed {
            self.bridge_error = error.into();
        } else {
            self.local.fail(error);
        }
    }
    pub fn command<H: Host<Tool = T>>(
        &mut self,
        host: &mut H,
        ctx: &egui::Context,
        id: &str,
    ) -> Option<Result<Value, String>> {
        // Settings remain available without starting or downloading a component.
        if id == "ai.settings" {
            return Some(host.open_preferences().map(|()| Value::Null));
        }
        if id == "ai.update" {
            self.updater.check(ctx_wake(ctx));
            return Some(Ok(json!({"pending":true})));
        }
        if matches!(
            id,
            "ai.send" | "ai.connection" | "ai.saveKey" | "ai.loadKey" | "ai.forgetKey"
        ) {
            self.requested = true;
        }
        self.poll(host, ctx);
        if self.failed {
            if id == "ai.connection" {
                if craft_ai_chat_native::component::candidates()
                    .iter()
                    .all(|p| self.attempted.contains(p))
                {
                    self.attempted.clear();
                }
                self.failed = false;
                self.bridge_error.clear();
                self.poll(host, ctx);
                return Some(Ok(Value::Null));
            }
            return Some(if id == "ai.status" {
                Ok(json!({"busy":false,"componentError":self.bridge_error}))
            } else {
                Err(self.bridge_error.clone())
            });
        }
        if self.connection.is_none() {
            return self.local.command(host, ctx, id);
        }
        if id == "ai.status" {
            let mut status = self.status.clone();
            status["busy"] = json!(self.busy());
            status["highlights"] = self.local.guidance.status();
            status["walkthrough"] = self.local.guidance.walkthrough_status();
            status["componentVersion"] =
                json!(self.connection.as_ref().map(|c| c.version.as_str()));
            status["componentProtocol"] = json!(runtime::PROTOCOL);
            status["componentUpdate"] = json!(self.updater.message());
            return Some(Ok(status));
        }
        if !matches!(
            id,
            "ai.send"
                | "ai.stop"
                | "ai.clear"
                | "ai.connection"
                | "ai.saveKey"
                | "ai.loadKey"
                | "ai.forgetKey"
                | "ai.clearHighlights"
        ) {
            return None;
        }
        if id == "ai.send" && self.sending.is_some() {
            return Some(Ok(json!({"pending":true})));
        }
        if matches!(id, "ai.stop" | "ai.clearHighlights") {
            self.local.dismiss_guidance();
            if let Some((epoch, id)) = self.guide.take()
                && let Some(connection) = &self.connection
            {
                let _ = connection.send(Input::Reply {
                    epoch,
                    id,
                    result: Err("The user dismissed this step".into()),
                });
            }
            if id == "ai.stop" {
                self.tools.clear();
            }
        }
        if id == "ai.send" {
            self.local.guidance.begin_turn();
        }
        let value = if id == "ai.send" {
            json!(self.local.prompt)
        } else {
            Value::Null
        };
        let result = self.action(host, id, value).map(|serial| {
            if id == "ai.send" {
                self.sending = Some((serial, self.local.prompt.clone()));
            }
            json!({"pending":true})
        });
        if id == "ai.forgetKey" {
            self.fields.remove("ai.key");
            super::clear_key_undo(ctx);
        }
        Some(result)
    }
    pub fn poll<H: Host<Tool = T>>(&mut self, host: &mut H, ctx: &egui::Context) {
        self.updater.poll();
        if self.local.busy() {
            self.source_mode = true;
            self.local.poll(host, ctx);
            return;
        }
        if self.connection.is_none()
            && !self.failed
            && !self.source_mode
            && host.credential_profile().is_some()
            && (host.chat_is_open() || self.requested)
            && let Some(path) = self.choose_component(craft_ai_chat_native::component::candidates())
        {
            self.attempted.push(path.clone());
            match Connection::start(path, ctx) {
                Ok(connection) => self.connection = Some(connection),
                Err(error) => {
                    self.failed = true;
                    self.bridge_error = error;
                }
            }
        }
        if self.connection.is_none() {
            if !self.failed {
                self.local.poll(host, ctx);
            }
            return;
        }
        let snapshot = self.snapshot(host);
        let encoded = serde_json::to_vec(&snapshot).unwrap_or_default();
        let ready = self.connection.as_ref().is_some_and(|c| c.ready);
        if ready
            && (self.sync_needed || encoded != self.snapshot)
            && let Some(connection) = &self.connection
            && connection.send(Input::Sync { snapshot }).is_ok()
        {
            self.snapshot = encoded;
            self.sync_needed = false;
        }
        let mut error = None;
        for _ in 0..16 {
            let packet = self
                .connection
                .as_ref()
                .and_then(|c| c.receive.try_recv().ok());
            let Some(packet) = packet else {
                break;
            };
            match packet {
                Err(e) => {
                    error = Some(e);
                    break;
                }
                Ok(Output::Ready {
                    protocol,
                    version,
                    capabilities,
                }) => {
                    if protocol != runtime::PROTOCOL
                        || !capabilities.iter().any(|v| v == runtime::SEND_RECEIPTS)
                    {
                        error = Some(
                            "Chat component is incompatible. The bundled chat remains available."
                                .into(),
                        );
                        break;
                    }
                    if let Some(c) = &mut self.connection {
                        c.ready = true;
                        c.version = version;
                        self.sync_needed = true;
                    }
                }
                Ok(Output::View {
                    epoch,
                    view,
                    status,
                    submitted,
                }) if epoch == self.epoch => {
                    if !status.is_object()
                        || crate::core::bounds::value_size(&status, runtime::MAX_VIEW).is_err()
                    {
                        error = Some("Chat component returned an invalid status packet".into());
                        break;
                    }
                    if let Err(e) = view.validate() {
                        error = Some(e);
                        break;
                    }
                    if let Some(receipt) = submitted {
                        self.receipt(receipt);
                    }
                    self.view = Some(view);
                    self.status = status;
                    self.markdown.clear_viewers();
                }
                Ok(Output::Tool { epoch, id, call }) => {
                    if epoch == self.epoch
                        && matches!(
                            call.name.as_str(),
                            "guide_ui" | "guide_state" | "highlight_ui"
                        )
                    {
                        let result = host.info().validate_call(&call).and_then(|()| {
                            match call.name.as_str() {
                                "highlight_ui" => {
                                    self.local.guidance.apply(host, &call.arguments).map(Some)
                                }
                                "guide_state" => {
                                    if call.arguments.as_object().is_some_and(|v| v.is_empty()) {
                                        crate::guidance::state(host).map(Some)
                                    } else {
                                        Err("guide_state takes no parameters".into())
                                    }
                                }
                                _ => self.local.guidance.start_step(host, &call.arguments).map(
                                    |_| {
                                        self.guide = Some((epoch, id));
                                        None
                                    },
                                ),
                            }
                        });
                        match result {
                            Ok(None) => {}
                            result => {
                                if let Some(c) = &self.connection {
                                    let _ = c.send(Input::Reply {
                                        epoch,
                                        id,
                                        result: result
                                            .and_then(|v| v.ok_or("No guidance result".into())),
                                    });
                                }
                            }
                        }
                        continue;
                    }
                    let result = if epoch != self.epoch {
                        Err("The active document changed; the tool was cancelled".into())
                    } else if self.tools.len() >= 64 {
                        Err("Too many pending chat tools".into())
                    } else {
                        host.info()
                            .validate_call(&call)
                            .and_then(|()| host.start_tool(&call))
                    };
                    match result {
                        Ok(tool) => self.tools.push((epoch, id, tool, host.instance())),
                        Err(e) => {
                            if let Some(c) = &self.connection {
                                let _ = c.send(Input::Reply {
                                    epoch,
                                    id,
                                    result: Err(e),
                                });
                            }
                        }
                    }
                }
                Ok(Output::Preference { epoch, key, value }) if epoch == self.epoch => {
                    if matches!(
                        key.as_str(),
                        "ai.attachFrames" | "ai.rememberKey" | "ai.rememberHistory" | "ai.model"
                    ) {
                        if let Err(e) = host.set_preference(&key, value) {
                            self.fail(&e);
                        }
                    } else {
                        self.fail("Chat component requested an unsupported preference");
                    }
                }
                Ok(Output::OpenPreferences) => {
                    if let Err(e) = host.open_preferences() {
                        self.fail(&e);
                    }
                }
                Ok(Output::Error { message }) => {
                    error = Some(message);
                    break;
                }
                _ => {}
            }
        }
        if let Some((epoch, id)) = self.guide {
            let result = self.local.guidance.step_result(host);
            if !matches!(result, Ok(None)) {
                self.guide = None;
                if let Some(c) = &self.connection {
                    let _ = c.send(Input::Reply {
                        epoch,
                        id,
                        result: result.and_then(|v| v.ok_or("No guidance result".into())),
                    });
                }
            }
        }
        let mut index = 0;
        while index < self.tools.len() {
            let Some((epoch, id, tool, instance)) = self.tools.get_mut(index) else {
                break;
            };
            let result = if !Arc::ptr_eq(instance, &host.instance()) {
                Err("The active document changed; the tool was cancelled".into())
            } else {
                host.poll_tool(tool)
            };
            match result {
                Ok(None) => index += 1,
                result => {
                    let result = result.and_then(|v| v.ok_or("Tool returned no result".into()));
                    let epoch = *epoch;
                    let id = *id;
                    if let Some(c) = &self.connection
                        && c.send(Input::Reply { epoch, id, result }).is_err()
                    {
                        error = Some("Chat pipe is full; the operation stopped safely".into());
                    }
                    self.tools.swap_remove(index);
                }
            }
        }
        if self
            .connection
            .as_ref()
            .is_some_and(|c| !c.ready && c.started.elapsed() > Duration::from_secs(10))
        {
            error = Some("Chat component did not finish its compatibility handshake".into());
        }
        if let Some(error) = error {
            self.connection = None;
            self.tools.clear();
            self.view = None;
            self.fields.clear();
            super::clear_key_undo(ctx);
            self.failed = true;
            self.bridge_error = error;
        }
        if self.connection.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
    pub fn show<H: Host<Tool = T>>(&mut self, host: &mut H, ui: &mut egui::Ui, rect: egui::Rect) {
        self.requested = true;
        self.poll(host, ui.ctx());
        egui::Panel::bottom("ai.component.update")
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let response = ui.small_button("Update chat component");
                    host.widget("ai.update", &response, "Update chat component");
                    if response.clicked() {
                        let _ = self.command(host, ui.ctx(), "ai.update");
                    }
                    ui.small(self.updater.message());
                });
            });
        if self.connection.is_none() {
            if self.failed {
                ui.label(&self.bridge_error);
                ui.label("Your saved conversation is preserved. Restart the chat component to recover it.");
                let response = ui.button("Restart chat component");
                host.widget("ai.connection", &response, "Restart chat component");
                if response.clicked() {
                    let _ = self.command(host, ui.ctx(), "ai.connection");
                }
                return;
            }
            self.local
                .show(host, ui, ui.available_rect_before_wrap().intersect(rect));
            return;
        }
        let Some(view) = self.view.take() else {
            ui.spinner();
            ui.label("Starting chat component…");
            return;
        };
        let epoch = self.epoch;
        self.show_view(host, ui, &view);
        // Rendering may poll a newer view or switch documents while dispatching an action.
        if self.view.is_none() && self.epoch == epoch {
            self.view = Some(view);
        }
    }
    fn show_view<H: Host<Tool = T>>(&mut self, host: &mut H, ui: &mut egui::Ui, view: &View) {
        let mut actions = Vec::new();
        egui::Panel::bottom("ai.remote.composer")
            .frame(egui::Frame::NONE)
            .show(ui, |ui| self.render(host, ui, &view.bottom, &mut actions));
        egui::Panel::top("ai.remote.toolbar")
            .frame(egui::Frame::NONE)
            .show(ui, |ui| self.render(host, ui, &view.top, &mut actions));
        self.local.guidance_ui(host, ui);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let scroll = egui::ScrollArea::vertical()
                    .id_salt("ai.remote.messages")
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| self.render(host, ui, &view.body, &mut actions));
                host.area("ai.messages", scroll.inner_rect, "AI conversation");
            });
        self.actions(host, ui.ctx(), actions);
    }
    pub fn credentials_ui<H: Host<Tool = T>>(
        &mut self,
        host: &mut H,
        ui: &mut egui::Ui,
        _preferences: bool,
    ) {
        self.requested = true;
        self.poll(host, ui.ctx());
        if self.failed {
            ui.label(&self.bridge_error);
            return;
        }
        if self.connection.is_none() {
            self.local.credentials_ui(host, ui, true);
            return;
        }
        if let Some(settings) = self.view.as_ref().map(|v| v.settings.clone()) {
            let mut actions = vec![];
            self.render(host, ui, &settings, &mut actions);
            self.actions(host, ui.ctx(), actions);
        }
    }
    fn actions<H: Host<Tool = T>>(
        &mut self,
        host: &mut H,
        ctx: &egui::Context,
        actions: Vec<(String, Value)>,
    ) {
        for (id, value) in actions {
            let result = if id == "ai.prompt" {
                Ok(())
            } else if id == "ai.attach" {
                // Apply a context opt-out before a Send in this same UI pass. A round trip
                // through the component could otherwise send the old attachment preference.
                host.set_preference("ai.attachFrames", value)
            } else if id == "ai.key" {
                self.action(host, &id, value).map(|_| ())
            } else {
                self.command(host, ctx, &id)
                    .unwrap_or_else(|| Err("Unknown chat action".into()))
                    .map(|_| ())
            };
            if let Err(error) = result {
                self.fail(&error);
            }
        }
        // Drafts are synchronized independently of sending, so Send availability is component-owned.
        if self.connection.is_some()
            && self.local.prompt != self.sent_prompt
            && self
                .action(host, "ai.prompt", json!(self.local.prompt))
                .is_ok()
        {
            self.sent_prompt = self.local.prompt.clone();
        }
    }
    fn render<H: Host<Tool = T>>(
        &mut self,
        host: &mut H,
        ui: &mut egui::Ui,
        nodes: &[Node],
        actions: &mut Vec<(String, Value)>,
    ) {
        for node in nodes {
            match node {
                Node::Separator => {
                    ui.separator();
                }
                Node::Row { children } => {
                    ui.horizontal_wrapped(|ui| self.render(host, ui, children, actions));
                }
                Node::Card {
                    id,
                    accent,
                    children,
                } => {
                    let fill = if *accent {
                        ui.visuals().selection.bg_fill.linear_multiply(0.2)
                    } else {
                        ui.visuals().faint_bg_color
                    };
                    let card = egui::Frame::new()
                        .fill(fill)
                        .corner_radius(8)
                        .inner_margin(10)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            self.render(host, ui, children, actions);
                        });
                    host.area(id, card.response.rect, "Chat card");
                    ui.add_space(6.0);
                }
                Node::Text { id, text, style } => {
                    if matches!(style, TextStyle::Markdown)
                        && let Some(markdown) = crate::conversation::prepare_markdown(text)
                    {
                        egui_commonmark::CommonMarkViewer::new().show_with_id(
                            egui::Id::unique(id),
                            ui,
                            &mut self.markdown,
                            &markdown,
                        );
                    } else {
                        let rich = match style {
                            TextStyle::Heading => egui::RichText::new(text).strong().size(17.0),
                            TextStyle::Muted => egui::RichText::new(text).small().weak(),
                            TextStyle::Warning => {
                                egui::RichText::new(text).color(ui.visuals().warn_fg_color)
                            }
                            _ => egui::RichText::new(text),
                        };
                        let response = ui.add(egui::Label::new(rich).wrap().selectable(true));
                        host.widget(id, &response, "Chat text");
                    }
                }
                Node::Button {
                    id,
                    label,
                    enabled,
                    primary,
                } => {
                    let mut button = egui::Button::new(label);
                    if *primary {
                        button = button.fill(ui.visuals().selection.bg_fill);
                    }
                    let response = ui.add_enabled(*enabled, button);
                    host.widget(id, &response, label);
                    if response.clicked() {
                        if id.ends_with(".copy") {
                            let prefix = id.trim_end_matches(".copy");
                            if let Some(Node::Text { text, .. }) = nodes.iter().find(
                                |n| matches!(n,Node::Text {id,..} if id==&format!("{prefix}.text")),
                            ) {
                                ui.ctx().copy_text(text.clone());
                            }
                        } else {
                            actions.push((id.clone(), Value::Null));
                        }
                    }
                }
                Node::Toggle {
                    id,
                    label,
                    value,
                    enabled,
                } => {
                    let mut value = *value;
                    let response = ui.add_enabled(*enabled, egui::Checkbox::new(&mut value, label));
                    host.widget(id, &response, label);
                    if response.changed() {
                        actions.push((id.clone(), json!(value)));
                    }
                }
                Node::Input {
                    id,
                    label,
                    value,
                    password,
                    multiline,
                    enabled,
                } => {
                    let draft = if id == "ai.prompt" {
                        &mut self.local.prompt
                    } else {
                        self.fields
                            .entry(id.clone())
                            .or_insert_with(|| value.clone())
                    };
                    let input_id = egui::Id::unique(if *password { "ai.key.field" } else { id });
                    let enter = *multiline
                        && ui.ctx().memory(|m| m.has_focus(input_id))
                        && ui.input_mut(|i| {
                            i.modifiers == egui::Modifiers::NONE
                                && i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                        });
                    let edit = if *multiline {
                        egui::TextEdit::multiline(draft).desired_rows(3).return_key(
                            egui::KeyboardShortcut::new(egui::Modifiers::SHIFT, egui::Key::Enter),
                        )
                    } else {
                        egui::TextEdit::singleline(draft)
                    };
                    let response = ui.add_enabled(
                        *enabled,
                        edit.id(input_id)
                            .password(*password)
                            .char_limit(if *password { 512 } else { 32768 })
                            .desired_width(f32::INFINITY)
                            .hint_text(label),
                    );
                    response
                        .ctx
                        .accesskit_node_builder(response.id, |node| node.set_label(label));
                    host.widget(id, &response, label);
                    if response.changed() {
                        actions.push((id.clone(), json!(draft)));
                    }
                    if enter {
                        actions.push(("ai.send".into(), Value::Null));
                    }
                }
            }
        }
    }
}
fn ctx_wake(ctx: &egui::Context) -> craft_ai_chat_native::Wake {
    let ctx = ctx.clone();
    Arc::new(move || ctx.request_repaint())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejected_or_stale_send_receipts_never_erase_a_draft() {
        let mut panel = Panel::<()>::default();
        panel.local.prompt = "Keep this instruction".into();
        panel.sending = Some((7, panel.local.prompt.clone()));
        panel.receipt(runtime::Receipt {
            serial: 6,
            accepted: true,
        });
        assert_eq!(panel.local.prompt, "Keep this instruction");
        assert!(panel.sending.is_some());
        panel.receipt(runtime::Receipt {
            serial: 7,
            accepted: false,
        });
        assert_eq!(panel.local.prompt, "Keep this instruction");
        assert!(panel.sending.is_none());
        panel.sending = Some((8, panel.local.prompt.clone()));
        panel.local.prompt = "A newer draft".into();
        panel.receipt(runtime::Receipt {
            serial: 8,
            accepted: true,
        });
        assert_eq!(panel.local.prompt, "A newer draft");
        panel.sending = Some((9, panel.local.prompt.clone()));
        panel.receipt(runtime::Receipt {
            serial: 9,
            accepted: true,
        });
        assert!(panel.local.prompt.is_empty());
    }
    #[test]
    fn installing_a_component_does_not_switch_an_existing_source_conversation() {
        let mut panel = Panel::<()>::default();
        assert!(panel.choose_component(vec![]).is_none());
        assert!(
            panel
                .choose_component(vec![PathBuf::from("new-component")])
                .is_none()
        );
        let mut fresh = Panel::<()>::default();
        assert_eq!(
            fresh.choose_component(vec![PathBuf::from("new-component")]),
            Some(PathBuf::from("new-component"))
        );
    }
    struct TestHost {
        info: crate::HostInfo,
        preferences: crate::Preferences,
        instance: Arc<()>,
        rects: BTreeMap<String, egui::Rect>,
    }
    impl Host for TestHost {
        type Tool = ();
        fn info(&self) -> &crate::HostInfo {
            &self.info
        }
        fn instance(&self) -> Arc<()> {
            self.instance.clone()
        }
        fn preferences(&self) -> crate::Preferences {
            self.preferences.clone()
        }
        fn start_tool(&mut self, _: &crate::ToolCall) -> Result<(), String> {
            Err("Unexpected edit".into())
        }
        fn poll_tool(&mut self, _: &mut ()) -> Result<Option<Value>, String> {
            Err("Unexpected edit".into())
        }
        fn set_preference(&mut self, key: &str, value: Value) -> Result<(), String> {
            if key == "ai.attachFrames" {
                self.preferences.attach_frames = value.as_bool().ok_or("Expected a boolean")?;
            }
            Ok(())
        }
        fn open_preferences(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn widget(&mut self, id: &str, r: &egui::Response, _: &str) {
            self.rects.insert(id.into(), r.rect);
        }
        fn area(&mut self, id: &str, rect: egui::Rect, _: &str) {
            self.rects.insert(id.into(), rect);
        }
    }
    #[test]
    fn attachment_opt_out_reaches_editor_preferences_before_the_next_action() {
        let mut host = TestHost {
            info: crate::core::editor::EditorTools {
                name: "test",
                methods: &["engine.commands"],
                views: &[],
            }
            .host_info("test", "Test", "test", "Attach"),
            preferences: Default::default(),
            instance: Arc::new(()),
            rects: BTreeMap::new(),
        };
        assert!(host.preferences().attach_frames);
        Panel::<()>::default().actions(
            &mut host,
            &egui::Context::default(),
            vec![("ai.attach".into(), json!(false))],
        );
        assert!(!host.preferences().attach_frames);
    }
    #[test]
    fn native_component_layout_keeps_composer_visible_and_formats_markdown() {
        let host = TestHost {
            preferences: Default::default(),
            info: crate::core::editor::EditorTools {
                name: "test",
                methods: &["engine.commands"],
                views: &[],
            }
            .host_info("test", "Test", "test", "Attach image"),
            instance: Arc::new(()),
            rects: BTreeMap::new(),
        };
        let view=View {
            top:vec![Node::Text {id:"title".into(),text:"Assistant".into(),style:TextStyle::Heading},Node::Text {id:"model".into(),text:"Native component · independently updated".into(),style:TextStyle::Muted}],
            body:vec![Node::Card {id:"reply".into(),accent:false,children:vec![Node::Text {id:"reply.text".into(),text:"### Ready to create\nDescribe an edit, then refine it with me.\n\n- **One conversation** across restarts\n- Editor actions use **Undo**\n\n`Tools` stay in readable activity cards.".into(),style:TextStyle::Markdown}]}],
            bottom:vec![Node::Input {id:"ai.prompt".into(),label:"Describe an edit…".into(),value:String::new(),password:false,multiline:true,enabled:true},Node::Button {id:"ai.send".into(),label:"Send".into(),enabled:true,primary:true}],settings:vec![],
        };
        view.validate().unwrap();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(360.0, 640.0))
            .build_ui_state(
                move |ui, (panel, host): &mut (Panel<()>, TestHost)| {
                    host.rects.clear();
                    panel.show_view(host, ui, &view);
                },
                (Panel::default(), host),
            );
        harness.run_steps(3);
        for id in ["ai.prompt", "ai.send", "ai.messages"] {
            let rect = harness.state().1.rects.get(id).unwrap();
            assert!(
                rect.is_finite() && rect.min.x >= 0.0 && rect.max.x <= 360.5 && rect.max.y <= 640.5,
                "{id}: {rect:?}"
            );
        }
        if let Some(path) = std::env::var_os("CRAFT_CHAT_SNAPSHOT") {
            harness.render().unwrap().save(path).unwrap();
        }
    }
}
