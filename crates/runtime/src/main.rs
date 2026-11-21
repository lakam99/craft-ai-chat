//! Isolated native chat service; stdout is reserved for the bounded bridge protocol.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    // Never print a panic payload: it could contain a chat, key, or tool result.
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(|| match std::env::args().nth(1).as_deref() {
        Some("--probe") => {
            use craft_ai_chat_egui::core::runtime::{self, Output};
            runtime::write(
                &mut std::io::stdout(),
                &Output::Ready {
                    protocol: runtime::PROTOCOL,
                    version: env!("CARGO_PKG_VERSION").into(),
                },
            )
        }
        Some("--stdio-v1") => craft_ai_chat_egui::service::run(),
        _ => Err("This component is started by a compatible Craft editor".into()),
    });
    if matches!(result, Ok(Ok(()))) {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}
