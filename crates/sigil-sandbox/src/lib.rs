//! RFC-0071: sigil-sandbox (R71.3 environment/receipt/enforcement foundation).
//!
//! The sandbox never creates or recycles authority resources, never guesses a writable root
//! from environment or cwd, never persists absolute paths into public events, and never
//! silently falls back to Local when required confinement is unmet.

pub mod backends;
pub mod environment;
pub mod launch_plan;
pub mod provider;
pub mod receipt;
pub mod toolchain;

pub use environment::{
    ExecutionTempEnvPlanV1, ReservedEnvKeyV1, ReservedEnvOverrideV1, apply_reserved_environment,
    standard_reserved_environment,
};
pub use provider::{SandboxProviderFactoryV1, SandboxProviderIdV1, SandboxProviderUnavailableV1};
pub use receipt::{
    EnforcementVerificationErrorV1, ReadIsolationCompletenessV1, ReadIsolationRequirementV1,
    SandboxPlatformSupportV1, verify_enforcement,
};
