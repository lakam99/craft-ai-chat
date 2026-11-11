use super::*;
fn name(response: &egui::Response, label: &str) {
    response
        .ctx
        .accesskit_node_builder(response.id, |node| node.set_label(label));
}
fn register<H: Host>(host: &mut H, id: &str, response: &egui::Response, label: &str) {
    host.widget(id, response, label);
}
impl<T> Panel<T> {
    pub fn credentials_ui<H: Host<Tool = T>>(
        &mut self,
        host: &mut H,
        ui: &mut egui::Ui,
        preferences: bool,
    ) {
        let ctx = ui.ctx().clone();
        self.sync(host, &ctx);
        self.initialize_vault(&ctx);
        let busy = self.pending.is_some() || self.vault.busy();
        let prefix = if preferences { "settings.ai" } else { "ai" };
        let field = if preferences {
            "ai.key.preferences"
        } else {
            "ai.key.field"
        };
        ui.label("OpenAI API key");
        let response = ui.add_enabled(
            !busy,
            egui::TextEdit::singleline(&mut self.key)
                .id(egui::Id::unique(field))
                .password(true)
                .char_limit(512)
                .desired_width(f32::INFINITY),
        );
        // Name the password control without exposing its value to the accessibility tree.
        name(&response, "OpenAI API key");
        register(host, &format!("{prefix}.key"), &response, "OpenAI API key");
        let mut action = None;
        ui.horizontal_wrapped(|ui| {
            let response = ui.add_enabled(
                !busy && !self.key.trim().is_empty() && self.preferences.remember_key,
                egui::Button::new("Save key securely"),
            );
            register(
                host,
                &format!("{prefix}.saveKey"),
                &response,
                "Save key securely",
            );
            if response.clicked() {
                action = Some("ai.saveKey");
            }
            let response = ui.add_enabled(
                !busy && self.preferences.remember_key,
                egui::Button::new("Load saved key"),
            );
            register(
                host,
                &format!("{prefix}.loadKey"),
                &response,
                "Load saved API key",
            );
            if response.clicked() {
                action = Some("ai.loadKey");
            }
            let response = ui.add_enabled(!busy, egui::Button::new("Forget key"));
            register(
                host,
                &format!("{prefix}.forgetKey"),
                &response,
                "Forget API key",
            );
            if response.clicked() {
                action = Some("ai.forgetKey");
            }
            if self.vault.busy() {
                ui.spinner();
            }
        });
        if self.preferences.remember_key {
            ui.small("Saved in the OS credential store, never in prefs.json. Sending also saves the entered key.");
        } else {
            ui.small(
                "Session only. Enable secure key saving in Preferences → AI Chat to remember it.",
            );
        }
        if !self.vault.message.is_empty() {
            ui.label(&self.vault.message);
        }
        if let Some(action) = action
            && let Some(Err(error)) = self.command(host, &ctx, action)
        {
            self.vault.message = error;
        }
    }

    /// A normal dock body: tabs, splitting, floating and closing use the shared panel system.
    pub fn show<H: Host<Tool = T>>(&mut self, host: &mut H, ui: &mut egui::Ui, rect: egui::Rect) {
        self.sync(host, ui.ctx());
        let mut child =
            ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(8.0, 6.0))));
        let ui = &mut child;
        let ctx = ui.ctx().clone();
        let busy = self.pending.is_some();
        let mut action = None;
        egui::Panel::bottom("ai.composer")
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let mut attach = self.preferences.attach_frames;
                let response = ui.add_enabled(
                    !busy,
                    egui::Checkbox::new(&mut attach, host.info().attachment_label.as_str()),
                );
                register(
                    host,
                    "ai.attach",
                    &response,
                    &host.info().attachment_label.clone(),
                );
                if response.changed()
                    && let Err(error) = host.set_preference("ai.attachFrames", json!(attach))
                {
                    self.message = error.to_string();
                }
                let response = ui.add_enabled(
                    !busy,
                    egui::TextEdit::multiline(&mut self.prompt)
                        .char_limit(32_768)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .hint_text("Describe what you want to do…"),
                );
                register(host, "ai.prompt", &response, "AI message");
                name(&response, "AI message");
                ui.horizontal_wrapped(|ui| {
                    let response = ui.add_enabled(
                        !busy && !self.vault.busy() && !self.prompt.trim().is_empty(),
                        egui::Button::new("Send"),
                    );
                    register(host, "ai.send", &response, "Send AI message");
                    if response.clicked() {
                        action = Some("ai.send");
                    }
                    let response = ui.add_enabled(busy, egui::Button::new("Stop"));
                    register(host, "ai.stop", &response, "Stop AI");
                    if response.clicked() {
                        action = Some("ai.stop");
                    }
                    let response = ui.add_enabled(!busy, egui::Button::new("Clear chat"));
                    register(host, "ai.clear", &response, "Clear AI chat");
                    if response.clicked() {
                        action = Some("ai.clear");
                    }
                    if busy {
                        ui.spinner();
                    }
                });
                ui.label(&self.message);
            });
        egui::Panel::top("ai.toolbar")
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("Model: {}", self.preferences.model));
                    let response = ui.button("Preferences…");
                    register(host, "ai.settings", &response, "AI Chat preferences");
                    if response.clicked() {
                        action = Some("ai.settings");
                    }
                });
            });
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
        let scroll =
            egui::ScrollArea::vertical().id_salt("ai.messages").min_scrolled_height(0.0).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                let connection =
                    egui::CollapsingHeader::new("Connection").id_salt("ai.connection").default_open(true).show(ui, |ui| self.credentials_ui(host, ui, false));
                register(host,"ai.connection", &connection.header_response, "OpenAI connection");
                ui.small("Messages and requested document context go to OpenAI. API usage is billed to your key. Use Undo to revert edits.");
                ui.separator();
                if self.transcript.is_empty() {
                    ui.label(&host.info().welcome);
                }
                for (index, (role, text)) in self.transcript.iter().enumerate() {
                    ui.strong(role);
                    let response = ui.add(egui::Label::new(text).wrap().selectable(true));
                    register(host,&format!("ai.message.{index}"), &response, role);
                    ui.add_space(7.0);
                }
            });
        host.area("ai.messages", scroll.inner_rect, "AI conversation");
    });
        if let Some(action) = action
            && let Some(Err(error)) = self.command(host, &ctx, action)
        {
            self.message = error;
        }
    }
}
