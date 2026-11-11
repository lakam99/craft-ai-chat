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
Chats remain in memory. Stop prevents further tool dispatch; completed edits
remain undoable and an in-flight HTTP request can take up to 120 seconds to end.
Tool calls are capped at 32 per turn, with bounded bodies/history and timeouts.
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
