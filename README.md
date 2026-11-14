# Craft AI Chat

A shared Rust plugin for docked AI chat in Craft applications. FilmCraft and
EffectCraft integrate the same panel through small, product-owned adapters.
Other editors can advertise their own tools and active-view context without
depending on either video editor.

This is a source integration, not a runtime plugin file for already-built apps.
The application must be rebuilt with an adapter.

## Packages

| Crate | Responsibility |
| --- | --- |
| `craft-ai-chat-core` | Host capabilities, preferences, bounded Responses function-calling loop. No network, OS or UI dependencies; supports WASM. |
| `craft-ai-chat-native` | Fixed OpenAI HTTPS endpoint, bounded requests, OS credential storage. |
| `craft-ai-chat-egui` | Shared dock body, chat state, password controls, cancellation and UI-thread tool dispatch. |

Desktop UI integration requires Rust 1.95 and egui 0.37. The workspace pins the
same permissively licensed egui revision used by the two editors. Cargo patches
in a dependency are not inherited: the consuming workspace must use a compatible
egui source/revision too. See the root `Cargo.toml` for all patch entries, including `egui_extras` used by the Markdown renderer.
Markdown uses `egui_commonmark` (MIT OR Apache-2.0) and `pulldown-cmark` (MIT);
license notices are included in `third_party/`. Image, file and network loading
features are disabled.

## Integrating a product

1. Add `craft-ai-chat-egui` as a Git dependency pinned to a reviewed commit in
   this repository. Keep it in native-target dependencies. Core-only users can
   depend on `craft-ai-chat-core` instead.
2. Own a `Panel<YourPendingTool>` alongside the application's session-only UI
   state. Do not serialize the panel, its prompt or transcript into project files.
3. Implement `Host`. Define a stable product ID, instructions and strict function
   schemas in `HostInfo`. `automatic_inputs` are optional calls that collect active
   document context when the attachment preference is enabled. They can return
   text, images or files; video is not required.
4. In `start_tool`, validate arguments and dispatch the application's existing
   commands. Never implement a second edit path. In `poll_tool`, return `None`
   until an asynchronous command or render completes, then its result. Both
   methods run on the UI thread; the pending tool need not be `Send`.
5. Return a stable `Arc<()>` for the current document from `instance()`. Replace
   it whenever the document/project is replaced. That prevents delayed calls from
   editing a newly opened document. Keep edits in the host's normal Undo history.
6. Map `Preferences` to normal editor preferences. The panel writes
   `ai.attachFrames`. The host's preferences page owns model and remember-key
   editing and can embed `credentials_ui`.
7. Provide `CredentialProfile` with the preference file path and a stable,
   product-specific service name. Keep these identical to existing deployments
   when migrating, so saved keys continue to load. Return `None` for a temporary
   session with no OS key storage.
8. Call `panel.poll(host, ctx)` during UI updates, including when the dock is
   hidden. Call `panel.show(host, ui, rect)` inside the application's dock system.
   Register widgets/areas with the host's automation system using `widget`/`area`.
   Route `ai.*` commands to `panel.command`; opening the dock remains host-owned.

`crates/egui/tests/hosts.rs` is a complete, compiled non-video adapter example:
it advertises page inspection and document renaming, applies Undo, and exercises
the shared panel. FilmCraft and EffectCraft retain only their own rendered-frame
and command adapters.

Tool results can be strings, JSON values, or Responses content arrays containing
`input_text`, `input_image` and `input_file`. Other JSON values are encoded as a
string for `function_call_output`. Automatic context calls should return content
arrays. Validate all arguments in the adapter; a function schema is not an
authorization boundary. Advertise only operations allowed for that user and
exclude AI controls, credentials and recursive AI invocation.

## Conversation panel

Replies and user messages support Markdown headings, emphasis, lists, blockquotes,
links, fenced code blocks and tables. Assistant messages have a Copy button.
Images and HTML are omitted, and links are limited to HTTP(S) or anchors. Oversized
or deeply nested Markdown falls back to selectable plain text.

Tool calls appear as compact activity cards with readable names and Working,
Done, Failed or Stopped states. Raw arguments and results stay in protocol history
for the model; they never become chat bubbles or readable transcript entries.
Override `Host::tool_label` to resolve `command_id(call)` through the product's
command registry. The existing `transcript()` and `ai.status` formats remain
compatible, with readable tool summaries.

The composer stays at the bottom of the dock. Enter sends; Shift+Enter inserts a
new line. Stop replaces Send while a request runs. New chat clears the session.
Connection controls are collapsed behind the key-status button (`ai.connection`);
model, frame and key-storage preferences remain in the editor's preferences.

## Visual guidance

Adapters opt in with `craft_ai_chat_core::editor::with_guidance(info)` and implement
`Host::highlight_target` using the editor's live widget registry. Call
`Panel::paint_guidance` after layout in each editor viewport. Return clipped widget
rectangles, and return `None` for controls that are no longer visible.

The assistant can then call `highlight_ui` with up to eight discovered widget IDs
and short action labels. Numbered outlines match the instructions in the chat;
they do not click controls, edit the document or consume pointer input. Guidance
follows layout changes, omits hidden controls, and clears on document replacement,
Stop or a new question. Clear highlights and Escape dismiss the current guide;
the same AI turn cannot restore a guide the user dismissed. Empty targets clears
guidance programmatically. Invalid or reserved AI targets are rejected before
changing an existing guide. “How do I” instructions ask the assistant to guide
instead of performing the edit.

### Interactive walkthroughs

Ask for a step-by-step guide to highlight **one control at a time**. `guide_ui`
shows the current instruction in Markdown and waits for the user before returning
to the AI. The AI then inspects the updated editor and chooses the next control,
including controls that only appeared after the preceding step. No API requests
are made while waiting for the user's action.

Adapters implement `Host::guide_observation` from current-pass widget responses
and `Host::guide_state` with small, read-only counters/state, excluding credentials
and private drafts. `guide_state` advertises the available fields. A step can wait
for a button activation, a completed value edit, a visibility transition, or a
state comparison. Use result comparisons for operations such as file imports:
opening or cancelling a dialog is not successful completion. Plain painted areas
without a widget response support visibility/state checks or manual confirmation.

**Pause guide** suspends advancement; **Resume guide** checks again. **I've done
this** explicitly reports user confirmation rather than automatic verification.
Clear highlights, Escape, Stop and document replacement cancel the walkthrough.
Each human step has a bounded 30-minute wait; normal backend tools keep their
short timeouts, and the existing per-turn tool/history limits still apply.

## Privacy and lifecycle

The key stays in session memory or the OS credential store, never ordinary
preferences. The panel has no `Serialize` or `Debug` implementation. Password
controls expose no credential value through editor automation; Forget also
clears the password fields' undo state. A host/profile change clears session
credentials and chat context. Normal document replacement resets conversation
context and blocks delayed edits.

Send transmits the prompt, conversation, advertised tools and enabled active-view
context to OpenAI. Requests use `store: false`, retain reasoning items for
stateless tool turns, and never redirect authorization to another endpoint.
Chats can be saved locally beside the editor preferences, scoped to each project.
The host supplies `document_key()` (normally the project path) and the
`remember_history` preference. API keys and image/file attachments are excluded
from archives. **New chat** clears that project's saved conversation; disable
**Remember chat history locally** for session-only use. Existing chats from older
versions cannot be recovered after their context was already discarded.

Tool-limit, network and worker failures preserve the original request and completed
tool results, so **continue** retains the task. Unanswered calls are explicitly
marked interrupted; current editor state must be inspected before retrying.
Older attachments are omitted from subsequent turns. At 96 KiB or 128 items,
the plugin requests a concise continuation summary (without tools) and replaces
the model context only after a valid summary succeeds. The readable transcript
stays separate. Summary requests use the selected model and count as API usage;
failed compaction retains the full context for retry.
Command discovery uses a small topic index and bounded filtered pages with complete
parameter schemas. Repeat full-index calls in a turn are redirected to filters. Old
saved catalogs are reduced locally before evaluating the summary threshold, while
user requests, editing results and the readable transcript stay intact. Summary-only
requests omit opaque editor reasoning and can retry output-token exhaustion once
with a larger bounded budget; incomplete summaries never replace history.
Opaque provider continuation state stays paired with its tool calls in local
archives, as required by reasoning models; it is never rendered as chat text.
When the model changes, tool facts become quoted historical context and the old
model's opaque state and protocol items are removed together.
Repeated failed compaction cannot grow recovery history indefinitely. At the
recovery budget, another message is refused with the draft and history intact.

Stop prevents further tool dispatch; completed edits
remain undoable and an in-flight HTTP request can take up to 120 seconds to end.
Tool calls are capped at 32 per turn, with bounded bodies/history and timeouts.
Each `batch_commands` call can execute up to 64 discovered engine commands, in
order, with one progress card and per-command results. It uses the normal host
dispatcher and Undo history, yields between commands, and stops on the first
error. A batch is not an atomic edit or a single Undo step. The full envelope is
validated before editing; AI controls and recursive batches remain forbidden.
Tools and transport workers have panic guards, which report failures to the panel.

Protocol reference: [OpenAI function calling](https://developers.openai.com/api/docs/guides/function-calling).

## Checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check -p craft-ai-chat-core --target wasm32-unknown-unknown
```

Tests use injected transports and a mock credential store. They make no paid API
calls. They cover protocol continuity, images, hostile parameters, tool limits,
custom document tools, UI-thread execution, document/host switching and password
forget behavior, Markdown safety, action-card progress and narrow layouts. Editor repositories additionally test actual engine/UI dispatch,
rendered active views and dock integration.

## License

MIT OR Apache-2.0. See `NOTICE`, `LICENSE-MIT` and `LICENSE-APACHE`.
