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
        Some("--licenses") => {
            use std::io::Write;
            std::io::stdout()
                .write_all(
                    concat!(
                        include_str!("../../../NOTICE"),
                        "\n",
                        include_str!("../../../LICENSE-MIT"),
                        "\n",
                        include_str!("../../../LICENSE-APACHE"),
                        "\n",
                        include_str!("../../../third_party/egui_commonmark-0.26.0-LICENSE.txt"),
                        "\n",
                        include_str!(
                            "../../../third_party/egui_commonmark_backend-0.26.0-LICENSE.txt"
                        ),
                        "\n",
                        include_str!("../../../third_party/pulldown-cmark-0.13.6-LICENSE.txt"),
                        "\n",
                        include_str!("../../../third_party/unicase-2.10.0-LICENSE.txt")
                    )
                    .as_bytes(),
                )
                .map_err(|_| "Cannot display license notices".to_string())
        }
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
