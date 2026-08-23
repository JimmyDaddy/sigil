//! RFC-0071: sigil-resource-authority contract-only scaffold (R71.1).
//!
//! R71.1 adds the crate to the workspace without physical allocators or OS spawn. It owns the
//! opaque sealed capsules / registration / actor ports referenced by the V3 contract; concrete
//! bootstrap, arena and journal implementations arrive in R71.2.
//!
//! The only constructor for SandboxBoundExecutionLeaseV1::issue_prepared_launch lives here
//! (spawn_protocol.rs); sigil-sandbox submits factory-attested evidence and a one-shot actor
//! sink, and never imports authority local types in the reverse direction.

pub mod provider_registry;
pub mod spawn_protocol;

pub use spawn_protocol::{PreparedSandboxLaunchV1, SandboxBoundExecutionLeaseV1};
