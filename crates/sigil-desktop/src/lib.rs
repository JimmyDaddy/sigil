//! Desktop-owned launcher and client boundary.
//!
//! The crate owns the local `sigil serve` child, per-launch bearer, bootstrap handshake, and
//! shutdown lifecycle. It deliberately does not depend on the kernel, runtime, TUI, or HTTP server
//! implementation. A future native shell can depend on this crate without exposing process or
//! credential primitives to its renderer.

mod launcher;
mod protocol;
mod secret;

pub use launcher::{
    DesktopLaunchError, DesktopLaunchRequest, DesktopLauncher, DesktopServerProcess,
    DesktopShutdownError, DesktopShutdownKind, DesktopShutdownReport,
};
pub use protocol::{DesktopServerAuthentication, DesktopServerCapabilities, DesktopServerInfo};
