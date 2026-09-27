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
mod process;
mod repo_language;
mod repository_walk;
mod service;
mod tools;
mod workspace;

pub use context::{
    CodeContextBuilder, CodeContextHit, LspContextSnapshot, LspContextSnapshotStatus, RepoMapEdge,
    RepoMapEdgeKind, RepoMapLite, RepoMapLiteOptions, RepoReferenceRef, RepoSourceFileRef,
    RepoSymbolKind, RepoSymbolRef, build_repo_map_lite,
};
pub use process::{
    LanguageServerLaunchPortV1, LanguageServerLaunchRequestV1, LanguageServerProcessIoV1,
};
pub use repository_walk::visit_repository_files;
pub use service::{CodeDiagnostic, CodeIntelligenceService, CodeLocation, CodeRange, CodeSymbol};
pub use tools::register_code_intelligence_tools;
pub use workspace::{
    EffectiveServerPlan, PlannedServerStatus, config_enabled, effective_server_plan,
};

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
