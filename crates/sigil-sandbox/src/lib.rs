//! RFC-0071: sigil-sandbox contract-only scaffold (R71.1).
//!
//! R71.1 adds the crate to the workspace with provider/actor *contracts only* (no platform
//! spawn, no mount/profile). R71.3 fills in Local / Seatbelt / Bubblewrap / Docker / Windows
//! providers. The sandbox never creates or recycles authority resources, never guesses a
//! writable root from environment or cwd, and never silently falls back to Local when required
//! confinement is unmet.

pub mod launch_plan;
pub mod provider;

pub use provider::{SandboxProviderFactoryV1, SandboxProviderIdV1, SandboxProviderUnavailableV1};
