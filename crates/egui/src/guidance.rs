use super::{Host, Panel};
use egui::{Align2, Color32, FontId, Id, LayerId, Order, Rect, Stroke, StrokeKind, pos2};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc};

/// An adapter supplies geometry from its current widget registry, never guessed coordinates.
pub struct HighlightTarget {
    pub rect: Rect,
    pub viewport: egui::ViewportId,
}

#[cfg(test)]
mod tests;
impl HighlightTarget {
    pub fn new(rect: Rect) -> Self {
        Self {
            rect,
            viewport: egui::ViewportId::ROOT,
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    id: String,
    label: String,
}
#[derive(Default)]
pub(super) struct Guidance {
    targets: Vec<Target>,
    visible: HashSet<String>,
    instance: Option<Arc<()>>,
    dismissed: bool,
}
impl Guidance {
    pub(super) fn clear(&mut self) {
        self.targets.clear();
        self.visible.clear();
        self.instance = None;
    }
    pub(super) fn dismiss(&mut self) {
        self.clear();
        self.dismissed = true;
    }
    pub(super) fn begin_turn(&mut self) {
        self.clear();
        self.dismissed = false;
    }
    pub(super) fn apply<H: Host>(&mut self, host: &H, args: &Value) -> Result<Value, String> {
        if self.dismissed {
            return Err("The user dismissed guidance for this turn".into());
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            targets: Vec<Target>,
        }
        core_size(args)?;
        let args: Args = serde_json::from_value(args.clone())
            .map_err(|_| "Expected targets containing only id and label")?;
        if args.targets.len() > 8 {
            return Err("Highlight at most eight controls at a time".into());
        }
        let mut seen = HashSet::new();
        for target in &args.targets {
            if target.id.is_empty()
                || target.id.len() > 256
                || target.label.trim().is_empty()
                || target.label.len() > 160
                || target
                    .id
                    .chars()
                    .chain(target.label.chars())
                    .any(char::is_control)
                || super::core::editor::is_chat_target(&target.id)
                || !seen.insert(&target.id)
            {
                return Err("Invalid, duplicate or reserved highlight target".into());
            }
            let widget = host.highlight_target(&target.id).ok_or_else(|| format!("Control '{}' is not visible. Inspect ui.elements and open its panel or menu first.", target.id))?;
            if !widget.rect.is_finite() || !widget.rect.is_positive() {
                return Err("The control has no visible screen area".into());
            }
        }
        // All targets are validated before replacing an existing guide.
        self.targets = args.targets;
        self.visible = self
            .targets
            .iter()
            .map(|target| target.id.clone())
            .collect();
        self.instance = Some(host.instance());
        Ok(
            json!({"ok":true,"highlighted":self.targets.iter().enumerate().map(|(n,t)|json!({"number":n+1,"id":t.id,"label":t.label})).collect::<Vec<_>>()}),
        )
    }
    pub(super) fn sync<H: Host>(&mut self, host: &H) {
        if self
            .instance
            .as_ref()
            .is_some_and(|old| !Arc::ptr_eq(old, &host.instance()))
        {
            self.clear();
        }
    }
    pub(super) fn status(&self) -> Value {
        json!(self.targets.iter().enumerate().map(|(n,t)|json!({"number":n+1,"id":t.id,"label":t.label,"visible":self.visible.contains(&t.id)})).collect::<Vec<_>>())
    }
}
fn core_size(args: &Value) -> Result<(), String> {
    super::core::bounds::value_size(args, 8 * 1024).map(|_| ())
}

impl<T> Panel<T> {
    /// Draw after the host has registered this pass's controls, in each editor viewport.
    /// These are paint shapes only: they never intercept pointer or keyboard input.
    pub fn paint_guidance<H: Host<Tool = T>>(&mut self, host: &H, ctx: &egui::Context) {
        self.guidance.sync(host);
        // Observe the original event even when an editor shortcut already consumed Escape.
        // Do not consume it here: cancelling the editor's current gesture must still work.
        if ctx.input(|i| {
            i.raw.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Key {
                        key: egui::Key::Escape,
                        pressed: true,
                        ..
                    }
                )
            })
        }) {
            self.guidance.dismiss();
        }
        if ctx.will_discard() {
            return;
        }
        // Chat bodies can precede their target panels. Only the completed, accepted layout
        // can determine visibility; partial/discarded passes must not report visible controls hidden.
        self.guidance.visible = self
            .guidance
            .targets
            .iter()
            .filter_map(|target| {
                host.highlight_target(&target.id)
                    .filter(|w| w.rect.is_finite() && w.rect.is_positive())
                    .map(|_| target.id.clone())
            })
            .collect();
        let painter = ctx.layer_painter(LayerId::new(
            Order::Tooltip,
            Id::unique("craft.ai.guidance"),
        ));
        let screen = ctx.content_rect();
        // Tiny or minimized viewports cannot fit a badge; avoid crossed clamp bounds.
        if !screen.is_finite() || screen.width() < 20.0 || screen.height() < 20.0 {
            return;
        }
        let visuals = ctx.global_style().visuals.clone();
        let accent = visuals.selection.stroke.color;
        for (number, target) in self.guidance.targets.iter().enumerate() {
            let Some(widget) = host.highlight_target(&target.id) else {
                continue;
            };
            if widget.viewport != ctx.viewport_id() || !widget.rect.is_finite() {
                continue;
            }
            let rect = widget.rect.intersect(screen);
            if !rect.is_positive() {
                continue;
            }
            painter.rect(
                rect.expand(3.0),
                3.0,
                Color32::TRANSPARENT,
                Stroke::new(2.5, accent),
                StrokeKind::Outside,
            );
            let badge_x = if rect.left() >= screen.left() + 26.0 {
                rect.left() - 14.0
            } else {
                rect.right() + 14.0
            };
            let center = pos2(
                badge_x.clamp(screen.left() + 10.0, screen.right() - 10.0),
                rect.center()
                    .y
                    .clamp(screen.top() + 10.0, screen.bottom() - 10.0),
            );
            painter.circle_filled(center, 10.0, visuals.selection.bg_fill);
            painter.circle_stroke(center, 10.0, Stroke::new(1.5, accent));
            painter.text(
                center,
                Align2::CENTER_CENTER,
                (number + 1).to_string(),
                FontId::proportional(12.0),
                visuals.text_color(),
            );
        }
    }
    pub(super) fn guidance_ui<H: Host<Tool = T>>(&mut self, host: &mut H, ui: &mut egui::Ui) {
        self.guidance.sync(host);
        if self.guidance.targets.is_empty() {
            return;
        }
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.strong("Highlighted in the editor");
                let clear = ui.small_button("Clear highlights");
                host.widget("ai.clearHighlights", &clear, "Clear editor highlights");
                if clear.clicked() {
                    self.guidance.dismiss();
                }
            });
            // A long guide must not push the composer or the dismissal button out of a small dock.
            egui::ScrollArea::vertical()
                .id_salt("ai.highlight.targets")
                .max_height(110.0)
                .min_scrolled_height(0.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (n, target) in self.guidance.targets.iter().enumerate() {
                        let visible = self.guidance.visible.contains(&target.id);
                        ui.label(format!(
                            "{}. {}{}",
                            n + 1,
                            target.label,
                            if visible { "" } else { " (control hidden)" }
                        ));
                    }
                });
            ui.small("Press Esc to dismiss. Highlighting does not perform the action.");
        });
    }
}
