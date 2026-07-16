//! Bounded, provider-neutral code intelligence for Sigil runtimes.
//!
//! The crate-root façade exposes request-local repository mapping, warm LSP context snapshots,
//! the shared code-intelligence service, tool registration, and Doctor planning. LSP framing,
//! process discovery, edit preparation, caches, and workspace path helpers remain private so
//! callers cannot bypass trust, confinement, or prepared-mutation boundaries.

#![deny(missing_docs)]

mod cache;
mod context;
mod discovery;
mod edit;
mod error;
mod language;
mod lsp;
mod prepared_mutation;
mod repo_language;
mod service;
mod tools;
mod workspace;

pub use context::{
    CodeContextBuilder, CodeContextHit, LspContextSnapshot, LspContextSnapshotStatus, RepoMapEdge,
    RepoMapEdgeKind, RepoMapLite, RepoMapLiteOptions, RepoReferenceRef, RepoSourceFileRef,
    RepoSymbolKind, RepoSymbolRef, build_repo_map_lite,
};
pub use service::{CodeDiagnostic, CodeIntelligenceService, CodeLocation, CodeRange, CodeSymbol};
pub use tools::{
    register_code_intelligence_tools, register_code_intelligence_tools_with_workspace_trust,
};
pub use workspace::{
    EffectiveServerPlan, PlannedServerStatus, config_enabled, effective_server_plan,
};

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
