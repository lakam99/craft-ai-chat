use super::*;

/// The adapter reads these flags after drawing a real widget. Editing includes keyboard focus
/// and pointer drags, so a value step waits for the user to finish rather than the first keystroke.
#[derive(Default, Clone, Copy)]
pub struct Observation {
    pub activated: bool,
    pub changed: bool,
    pub editing: bool,
}
impl From<&egui::Response> for Observation {
    fn from(response: &egui::Response) -> Self {
        Self {
            activated: response.clicked(),
            changed: response.changed(),
            editing: response.has_focus() || response.dragged(),
        }
    }
}

#[derive(Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Completion {
    Activate,
    Change,
    Appear,
    Disappear,
    State,
    Confirm,
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Comparison {
    Equal,
    NotEqual,
    Greater,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    id: String,
    label: String,
    instruction: String,
    completion: Completion,
    watch_id: String,
    state_pointer: String,
    comparison: Comparison,
    expected_json: String,
}
pub(super) struct Step {
    args: Args,
    expected: Value,
    number: usize,
    armed_frame: Option<(egui::ViewportId, u64)>,
    changed: bool,
    observed: bool,
    confirmed: bool,
    paused: bool,
    consumed: bool,
    failure: Option<String>,
}
pub(crate) fn state<H: Host>(host: &H) -> Result<Value, String> {
    let value = host.guide_state();
    // The snapshot is polled on the UI thread; it must remain small and contain no drafts or keys.
    super::super::core::bounds::value_size(&value, 16 * 1024)?;
    Ok(value)
}
fn matches(value: &Value, expected: &Value, comparison: Comparison) -> bool {
    match comparison {
        Comparison::Equal => value == expected,
        Comparison::NotEqual => value != expected,
        Comparison::Greater => value
            .as_f64()
            .zip(expected.as_f64())
            .is_some_and(|(a, b)| a > b),
    }
}
impl Guidance {
    pub(crate) fn waiting_for_user(&self) -> bool {
        self.step.as_ref().is_some_and(|s| !s.consumed)
    }
    pub(crate) fn finish_walkthrough(&mut self) {
        if self.step.is_some() {
            self.clear();
        }
    }
    pub(crate) fn number(&self, index: usize) -> usize {
        self.step
            .as_ref()
            .map_or(index.saturating_add(1), |s| s.number)
    }
    pub(crate) fn start_step<H: Host>(
        &mut self,
        host: &H,
        value: &Value,
    ) -> Result<String, String> {
        core_size(value)?;
        let args: Args =
            serde_json::from_value(value.clone()).map_err(|_| "Invalid walkthrough step")?;
        if args.instruction.trim().is_empty()
            || args.instruction.len() > 4096
            || args.watch_id.len() > 256
            || args.watch_id.chars().any(char::is_control)
            || super::super::core::editor::is_chat_target(&args.watch_id)
            || args.state_pointer.len() > 256
            || args.expected_json.len() > 1024
        {
            return Err(
                "Walkthrough text or condition exceeds its limits or targets AI controls".into(),
            );
        }
        if self.step_number >= 24 {
            return Err("Guide step limit reached. Send a new question to continue.".into());
        }
        let expected: Value = if args.completion == Completion::State {
            let expected: Value = serde_json::from_str(&args.expected_json)
                .map_err(|_| "expected_json must be a JSON scalar")?;
            if expected.is_array() || expected.is_object() || !args.state_pointer.starts_with('/') {
                return Err("Use a JSON pointer and scalar expectation from guide_state".into());
            }
            let snapshot = state(host)?;
            let current = snapshot
                .pointer(&args.state_pointer)
                .ok_or("State field is not advertised by guide_state")?;
            if matches(current, &expected, args.comparison) {
                return Err("This step's result is already satisfied. Inspect the editor and choose the next step.".into());
            }
            if matches!(args.comparison, Comparison::Greater)
                && (!current.is_number() || !expected.is_number())
            {
                return Err("Greater comparisons require numeric state".into());
            }
            expected
        } else {
            if !args.state_pointer.is_empty() || !args.expected_json.is_empty() {
                return Err("State fields must be empty for widget or confirmation steps".into());
            }
            Value::Null
        };
        let watch = if args.watch_id.is_empty() {
            &args.id
        } else {
            &args.watch_id
        };
        if args.completion == Completion::Appear && host.highlight_target(watch).is_some() {
            return Err("The watched control is already visible".into());
        }
        if args.completion == Completion::Disappear && host.highlight_target(watch).is_none() {
            return Err("The watched control is already hidden".into());
        }
        if matches!(args.completion, Completion::Activate | Completion::Change)
            && !args.watch_id.is_empty()
            && args.watch_id != args.id
        {
            return Err("Observe the highlighted control for activate/change steps".into());
        }
        // Preserve a valid previous guide if any argument or the first target is invalid.
        self.apply(
            host,
            &json!({"targets":[{"id":args.id,"label":args.label}]}),
        )?;
        self.step_number = self.step_number.saturating_add(1);
        let text = format!("### Step {}\n\n{}", self.step_number, args.instruction);
        self.step = Some(Step {
            args,
            expected,
            number: self.step_number,
            armed_frame: None,
            changed: false,
            observed: false,
            confirmed: false,
            paused: false,
            consumed: false,
            failure: None,
        });
        Ok(text)
    }
    pub(crate) fn observe_step<H: Host>(&mut self, host: &H, ctx: &egui::Context) {
        let Some(step) = self.step.as_mut() else {
            return;
        };
        if step.paused || step.consumed {
            return;
        }
        let event = host.guide_observation(&step.args.id, ctx);
        if matches!(
            step.args.completion,
            Completion::Activate | Completion::Change
        ) && event.is_none()
        {
            return;
        }
        if !matches!(
            step.args.completion,
            Completion::Activate | Completion::Change
        ) && ctx.viewport_id() != egui::ViewportId::ROOT
        {
            return;
        }
        let frame = (ctx.viewport_id(), ctx.cumulative_frame_nr());
        match step.armed_frame {
            None => {
                step.armed_frame = Some(frame);
                return;
            }
            Some(first) if first.0 != frame.0 => {
                step.armed_frame = Some(frame);
                return;
            }
            Some(first) if first.1 == frame.1 => return,
            _ => {}
        }
        let watch = if step.args.watch_id.is_empty() {
            &step.args.id
        } else {
            &step.args.watch_id
        };
        let visible = || {
            host.highlight_target(watch)
                .is_some_and(|t| t.rect.is_finite() && t.rect.is_positive())
        };
        step.observed |= match step.args.completion {
            Completion::Activate => event.is_some_and(|e| e.activated),
            Completion::Change => {
                if let Some(event) = event {
                    step.changed |= event.changed;
                    step.changed && !event.editing
                } else {
                    false
                }
            }
            Completion::Appear => !ctx.will_discard() && visible(),
            Completion::Disappear => !ctx.will_discard() && !visible(),
            Completion::State => {
                if ctx.will_discard() {
                    false
                } else {
                    match state(host) {
                        Ok(snapshot) => match snapshot.pointer(&step.args.state_pointer) {
                            Some(value) => matches(value, &step.expected, step.args.comparison),
                            None => {
                                step.failure=Some("The watched state field is no longer available. Inspect the editor again.".into());
                                false
                            }
                        },
                        Err(error) => {
                            step.failure = Some(error);
                            false
                        }
                    }
                }
            }
            Completion::Confirm => false,
        };
    }
    pub(crate) fn step_result<H: Host>(&mut self, host: &H) -> Result<Option<Value>, String> {
        let Some(step) = self.step.as_mut() else {
            return Err("The guide was cancelled".into());
        };
        if let Some(error) = step.failure.take() {
            return Err(error);
        }
        if step.paused || step.consumed || !(step.observed || step.confirmed) {
            return Ok(None);
        }
        step.consumed = true;
        Ok(Some(
            json!({"ok":true,"step":step.number,"completed_by":if step.confirmed {"user_confirmation"} else {"observed"},"editor_state":state(host)?,"next":"Inspect ui.elements and guide_state again. Highlight only the next step; do not perform the edit for the user."}),
        ))
    }
    pub(crate) fn walkthrough_status(&self) -> Value {
        self.step.as_ref().map_or(Value::Null, |s|json!({"step":s.number,"target":s.args.id,"paused":s.paused,"completed":s.consumed,"waiting":!s.consumed}))
    }
}
impl<T> Panel<T> {
    pub(crate) fn dismiss_guidance(&mut self) {
        if self.guidance.step.is_some()
            && let Some(pending) = &self.pending
        {
            pending
                .cancel
                .store(true, std::sync::atomic::Ordering::Release);
        }
        self.guidance.dismiss();
    }
    pub(crate) fn walkthrough_controls<H: Host<Tool = T>>(
        &mut self,
        host: &mut H,
        ui: &mut egui::Ui,
    ) {
        let Some(step) = self.guidance.step.as_mut() else {
            return;
        };
        if step.consumed {
            ui.small("Step complete. Finding the next control…");
            return;
        }
        ui.small(if step.paused {
            "Guide paused"
        } else {
            "Waiting for your action. The AI will continue after this step."
        });
        ui.horizontal_wrapped(|ui| {
            let pause = ui.small_button(if step.paused { "Resume guide" } else { "Pause guide" });
            host.widget("ai.pauseGuide", &pause, "Pause or resume guide");
            if pause.clicked() { step.paused = !step.paused; }
            let done = ui.add_enabled(!step.paused, egui::Button::new("I've done this").small()).on_hover_text("Continue with your confirmation when automatic detection is unavailable. This is not reported as automatic verification.");
            host.widget("ai.confirmGuide", &done, "Confirm guide step");
            if done.clicked() { step.confirmed = true; }
        });
    }
}
