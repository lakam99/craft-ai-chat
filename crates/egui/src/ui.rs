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

    /// The editor owns docking; this body keeps setup, conversation and composing separate.
    pub fn show<H: Host<Tool = T>>(&mut self, host: &mut H, ui: &mut egui::Ui, rect: egui::Rect) {
        self.sync(host, ui.ctx());
        self.initialize_vault(ui.ctx());
        let mut child =
            ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(8.0, 6.0))));
        let ui = &mut child;
        let ctx = ui.ctx().clone();
        // Evicted messages must not leave renderer caches growing for the whole session.
        let oldest = self.chat.entries.first().map(|entry| entry.id);
        if oldest != self.markdown_oldest {
            self.markdown_cache.clear_viewers();
            self.markdown_oldest = oldest;
        }
        let busy = self.busy();
        let mut action = None;
        let prompt_id = egui::Id::unique("ai.prompt.field");
        let enter = !busy
            && ctx.memory(|m| m.has_focus(prompt_id))
            && ui.input_mut(|i| {
                // consume_key permits extra modifiers; preserve Shift+Enter for multiline input.
                i.modifiers == egui::Modifiers::NONE
                    && i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
            });

        egui::Panel::bottom("ai.composer")
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                ui.add_space(8.0);
                let frame = egui::Frame::new()
                    .fill(ui.visuals().faint_bg_color)
                    .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
                    .corner_radius(8)
                    .inner_margin(8);
                frame.show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("ai.prompt.scroll")
                        .max_height(104.0)
                        .show(ui, |ui| {
                            let response = ui.add_enabled(
                                !busy,
                                egui::TextEdit::multiline(&mut self.prompt)
                                    .id(prompt_id)
                                    .frame(egui::Frame::NONE)
                                    .char_limit(32_768)
                                    .desired_rows(3)
                                    .return_key(egui::KeyboardShortcut::new(
                                        egui::Modifiers::SHIFT,
                                        egui::Key::Enter,
                                    ))
                                    .desired_width(f32::INFINITY)
                                    .hint_text("Ask about your work, or describe an edit…"),
                            );
                            name(&response, "AI message");
                            register(host, "ai.prompt", &response, "AI message");
                        });
                    ui.separator();
                    let mut attach = self.preferences.attach_frames;
                    let label = host.info().attachment_label.clone();
                    let response = ui.add_enabled(!busy, egui::Checkbox::new(&mut attach, &label));
                    register(host, "ai.attach", &response, &label);
                    if response.changed()
                        && let Err(error) = host.set_preference("ai.attachFrames", json!(attach))
                    {
                        self.message = error;
                    }
                    ui.horizontal_wrapped(|ui| {
                        if busy {
                            let stopping = self
                                .pending
                                .as_ref()
                                .is_some_and(|p| p.cancel.load(Ordering::Acquire));
                            let response = ui.add_enabled(
                                !stopping,
                                egui::Button::new(if stopping { "Stopping…" } else { "Stop" }),
                            );
                            register(host, "ai.stop", &response, "Stop AI");
                            if response.clicked() {
                                action = Some("ai.stop");
                            }
                            if !self.guidance.waiting_for_user() {
                                ui.spinner();
                            }
                            ui.small(if stopping {
                                "Stopping…"
                            } else if self.guidance.waiting_for_user() {
                                "Waiting for you…"
                            } else {
                                "Working in the editor…"
                            });
                        } else {
                            let response = ui.add_enabled(
                                !self.vault.busy() && !self.prompt.trim().is_empty(),
                                egui::Button::new("Send").fill(ui.visuals().selection.bg_fill),
                            );
                            register(host, "ai.send", &response, "Send AI message");
                            if response.clicked() || enter {
                                action = Some("ai.send");
                            }
                            ui.small("Enter to send · Shift+Enter for a new line");
                        }
                    });
                });
            });
        egui::Panel::top("ai.toolbar").frame(egui::Frame::NONE).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("Assistant").strong().size(17.0));
                let response = ui.add_enabled(!busy, egui::Button::new("New chat").small());
                register(host, "ai.clear", &response, "New chat");
                if response.clicked() { action = Some("ai.clear"); }
                let response = ui.small_button("Preferences…");
                register(host, "ai.settings", &response, "AI Chat preferences");
                if response.clicked() { action = Some("ai.settings"); }
            });
            ui.add(egui::Label::new(egui::RichText::new(&self.preferences.model).small().weak()).truncate());
            let state = if self.vault.busy() { "Loading key…" } else if self.has_key() { "API key ready" } else { "Set up API key" };
            let response = ui.small_button(state);
            register(host, "ai.connection", &response, "OpenAI connection");
            if response.clicked() { self.connection_open = !self.connection_open; }
            if self.connection_open {
                ui.add_space(6.0);
                egui::Frame::group(ui.style()).corner_radius(6).show(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("ai.connection.scroll").max_height((rect.height() * 0.35).clamp(0.0, 200.0)).show(ui, |ui| {
                        self.credentials_ui(host, ui, false);
                        ui.small("Messages and selected context go to OpenAI. API usage is billed to your key.");
                    });
                    let response = ui.small_button("Close connection settings");
                    register(host, "ai.connection.close", &response, "Close connection settings");
                    if response.clicked() { self.connection_open = false; }
                });
            }
            ui.add_space(8.0);
            self.guidance_ui(host, ui);
            ui.separator();
        });
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
            let scroll = egui::ScrollArea::vertical().id_salt("ai.messages")
                .min_scrolled_height(0.0).auto_shrink([false, false]).stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 10.0;
                    if self.chat.entries.is_empty() {
                        ui.add_space(18.0);
                        ui.heading("What would you like to create?");
                        ui.label(&host.info().welcome);
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("Ask a question, describe an edit, or ask the assistant to inspect your work. Actions use the editor’s tools and Undo.").weak());
                        if !self.has_key() {
                            ui.add_space(8.0);
                            let response = ui.button("Add API key");
                            register(host, "ai.setup", &response, "Add API key");
                            if response.clicked() { self.connection_open = true; }
                        }
                    }
                    for entry in &self.chat.entries {
                        render_entry(host, ui, entry, &mut self.markdown_cache);
                    }
                    if busy && !self.chat.entries.iter().any(|entry| matches!(&entry.content, conversation::Content::Activity(a) if a.state == ActivityState::Running)) {
                        ui.horizontal(|ui| { ui.spinner(); ui.label(egui::RichText::new("Thinking…").weak()); });
                    }
                });
            host.area("ai.messages", scroll.inner_rect, "AI conversation");
        });
        if let Some(action) = action
            && let Some(Err(error)) = self.command(host, &ctx, action)
        {
            if action == "ai.send" && craft_ai_chat_core::validate_key(self.key.trim()).is_err() {
                self.connection_open = true;
            }
            self.message = error.clone();
            self.record("Notice", error);
        }
    }
}

fn render_entry<H: Host>(
    host: &mut H,
    ui: &mut egui::Ui,
    entry: &conversation::Entry,
    cache: &mut egui_commonmark::CommonMarkCache,
) {
    use conversation::Content;
    let id = entry.id;
    match &entry.content {
        Content::Activity(activity) => {
            let frame = egui::Frame::new()
                .fill(ui.visuals().faint_bg_color)
                .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
                .corner_radius(6)
                .inner_margin(8);
            let card = frame.show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    if activity.state == ActivityState::Running {
                        ui.spinner();
                    }
                    let color = match activity.state {
                        ActivityState::Failed => ui.visuals().error_fg_color,
                        _ => ui.visuals().weak_text_color(),
                    };
                    ui.label(
                        egui::RichText::new(activity.state.label())
                            .small()
                            .color(color),
                    );
                    ui.label(egui::RichText::new(&activity.title).strong());
                });
                if !activity.detail.is_empty() {
                    ui.add(
                        egui::Label::new(egui::RichText::new(&activity.detail).small())
                            .wrap()
                            .selectable(true),
                    );
                }
            });
            host.area(
                &format!("ai.activity.{id}"),
                card.response.rect,
                &activity.title,
            );
        }
        Content::Message {
            role,
            text,
            markdown,
        } => {
            let user = role == "You";
            let notice = role == "Notice";
            ui.horizontal(|ui| {
                if user {
                    ui.add_space((ui.available_width() * 0.08).min(24.0));
                }
                let fill = if user {
                    ui.visuals().selection.bg_fill.linear_multiply(0.2)
                } else {
                    ui.visuals().faint_bg_color
                };
                let card = ui
                    .vertical(|ui| {
                        egui::Frame::new()
                            .fill(fill)
                            .corner_radius(8)
                            .inner_margin(10)
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(egui::RichText::new(role).small().strong().color(
                                        if notice {
                                            ui.visuals().warn_fg_color
                                        } else {
                                            ui.visuals().weak_text_color()
                                        },
                                    ));
                                    if role == "Assistant" {
                                        let response = ui.small_button("Copy");
                                        register(
                                            host,
                                            &format!("ai.message.{id}.copy"),
                                            &response,
                                            "Copy assistant message",
                                        );
                                        if response.clicked() {
                                            ui.ctx().copy_text(text.clone());
                                        }
                                    }
                                });
                                ui.add_space(3.0);
                                if let Some(formatted) = markdown {
                                    egui_commonmark::CommonMarkViewer::new().show_with_id(
                                        egui::Id::unique(("ai.markdown", id)),
                                        ui,
                                        cache,
                                        formatted,
                                    );
                                } else {
                                    ui.add(egui::Label::new(text).wrap().selectable(true));
                                }
                            })
                    })
                    .inner;
                host.area(&format!("ai.message.{id}"), card.response.rect, role);
            });
        }
    }
}
