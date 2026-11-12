//! Native transport and OS vault. No editor or egui dependency, and no plaintext credential fallback.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod credentials;
pub mod history;
mod transport;
pub use transport::http;

/// Wake the host's event loop after a credential job completes.
pub type Wake = std::sync::Arc<dyn Fn() + Send + Sync>;
