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
The handshake advertises `send-receipts`. Action serials and send receipts ensure
the editor clears a draft only after the service accepts it. Older editors can
ignore these additive fields; this bridge requires the capability before use.
Hosts advertise `component-actions` to allow new chat controls and actions in
component releases. Interactive IDs stay in the reserved `ai.*` namespace and
are forwarded privately to the component, rather than to editor command dispatch.
Removed inputs release their transient drafts; a component can reset a form by
assigning a new input ID. Editor mutations wait for Send acceptance.

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

Hosts call the bridge's `poll` after an accepted editor layout, or before clearing
the previous completed layout's widget registry. Rendering the chat and reading
its status only pump transport and views; they defer editor tools to that explicit
poll, so a new walkthrough step never resolves against a half-drawn workspace.

## Settings and continuity

### Goals, history and interruptions (0.6.3)

**Pursue a goal** applies to the current chat. Describe both the task and what
must be true when it is finished. The assistant continues across tool-budget
segments while making progress. Repeated unchanged observations pause it;
questions, permissions and subjective judgments use a visible waiting state.
Completion requires a fresh editor readback and a separate verification request
against the exact user criteria and saved execution receipts. Verification is
evidence-based model judgment, not proof of arbitrary natural-language claims.

**Pause** prevents the next edit. An action already submitted is polled to its
actual result; a batch finishes its current command and leaves later commands
untouched. **Send correction** accepts steering while the assistant is busy,
keeps the draft until acceptance, and continues at that safe boundary. Up to
eight corrections / 32 KiB can wait at once; exceeding this retains the draft.
A pending HTTPS request can take up to its existing 120-second timeout to return.

**History** lists and searches chats for the open project. **New chat** retains
the previous conversation. Each chat has a separate bounded archive and the
catalog holds up to 512 chats without silently deleting older ones. Existing
single-chat archives migrate on first use. Previously cleared conversations
cannot be recovered. Saving can be disabled in editor preferences; continuous
goals require it. Restored goals always wait for **Resume**, including goals
that were running when the editor closed. Queued corrections also wait.

Exact goal instructions and subsequent user steering are stored separately
from model summaries. Each dispatched goal tool first writes a pending action;
its actual result is then saved as an immutable, checksummed receipt before
another editor action can start. A writer lease prevents concurrent runners of
the same goal. Checkpoints use atomic replacement and preserve a recovery copy.
If context summarization fails, reproducible context is rebuilt from command
knowledge, the canonical goal and receipt references; summaries cannot replace
user criteria. The local `goal_memory` tool retrieves older receipts on demand.
Bounded intent/receipt quotas fail explicitly instead of dropping instructions.

An interrupted process, unknown tool outcome, damaged checkpoint or changed
project blocks automatic resumption. Inspect the editor and acknowledge an
uncertain edit before resuming; the acknowledgment records uncertainty and
never labels the action successful or replays it. Disk/validation failures stop
further goal edits and preserve existing files. API keys are redacted from local
instructions, archives and receipts and remain in the OS credential store.

These controls use existing protocol-1 component actions and need no new editor
tools. Current ArkCraft editor bridges can install this runtime independently.
Source-integrated adapters receive the same UI when rebuilding against 0.6.3.

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

The update check uses the running component's completed handshake, not the
activation pointer, to decide whether a restart is needed. Checking the running
release reports that chat is up to date without reinstalling or restarting it.
A newer running version is never downgraded to an older published release. The
host footer shows the active version and disables repeated checks while busy.

Build the component with `cargo build --release -p craft-ai-chat-runtime` and
bundle the platform-named executable beside an editor executable. Runtime assets
are separate from editor releases. Only publish platforms actually built and
tested; source support does not establish a tested binary release.
The standalone executable also carries the project's distribution notices and
Markdown dependency license texts; display them with `--licenses`.
