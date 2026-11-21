# Independently updated native chat

FilmCraft, EffectCraft, SoundCraft and PhotoCraft use the same native Rust
component. The editor continues to own docking, preferences, the document, Undo,
automation permissions and command execution. ArkCraft Launcher can install the
shared component without reinstalling any editor.

An editor needs one initial update to adopt `craft_ai_chat_egui::client::Panel`.
After that, chat releases can change the layout, messages, Markdown presentation,
AI requests, context compression, command knowledge and batching independently.
New editor tools or new bridge primitives still require an editor update.

## Process boundary

The editor starts the component lazily with `--stdio-v1`, using private inherited
stdin/stdout pipes. There is no listener or public port. Protocol version 1 uses
bounded JSON frames for host snapshots, actions, views and tool replies. This is
an internal transport: tool payloads are never displayed as chat messages.

The service owns conversation logic, archives, secure credential access and a
declarative view tree. The editor renders the tree using native egui controls:
cards, rows, Markdown, accessible inputs, toggles and buttons. Keyboard focus,
IME and clipboard work in the editor normally. The component controls the panel's
design through this tree, rather than shipping only backend updates.

Every editor tool is validated against the host's advertised capabilities and
then dispatched through its existing adapter and automation policy. Document
epochs reject stale edits. Highlights and walkthrough observations resolve in
the editor using current widget geometry, not coordinates from the service.
There is no arbitrary shell-command tool.

## Settings and continuity

Model, attachments, remember-key and remember-history preferences remain in the
editor's preferences. The credential service and preference profile identity
remain unchanged, so saved keys and per-document archives continue to work.
The service never includes API keys in a view or status. Password drafts are
transient pipe data; neither protocol packets nor keys are logged.

Each editor runs its own component process, retaining its own document and
credential scope. Only the installed executable is shared. Active conversations
are never replaced by an update. Restart the editor when convenient to activate
an installed component. If a component fails, the panel offers a restart using
the previous compatible or bundled version and leaves saved history intact.
Offline installations without a component retain the source-integrated panel.

## Delivery

The panel's **Update chat component** control and ArkCraft Launcher's Settings
download only from `lakam99/craft-ai-chat` GitHub releases over HTTPS. A release
contains `chat-component-v1.json`, with `protocol`, `version` and an `assets` array
of `name`, `sha256`, and `size`. Assets use fixed platform-specific names, such as
`craft-ai-chat-runtime-windows-x64.exe`. Downloads are bounded, checked against
the release checksum and probed for protocol compatibility before activation.
Trust is the configured GitHub repository over HTTPS; the manifest is not a
separate cryptographic publisher signature.

Executables install into the user's ArkCraft component data folder, outside app
installers. Versioned folders avoid replacing running executables. Activation
keeps the previous manifest and bundled component as recovery candidates. No
project, preference archive or saved credential is moved by component updates.

Build the component with `cargo build --release -p craft-ai-chat-runtime` and
bundle the platform-named executable beside an editor executable. Runtime assets
are separate from editor releases. Only publish platforms actually built and
tested; source support does not establish a tested binary release.
The standalone executable also carries the project's distribution notices and
Markdown dependency license texts; display them with `--licenses`.
