pub mod cache;
pub mod context;
pub mod discovery;
pub mod edit;
pub mod error;
pub mod language;
pub mod lsp;
pub mod service;
pub mod tools;
pub mod workspace;

pub use context::{
    CodeContextBuilder, CodeContextHit, LspContextSnapshot, LspContextSnapshotStatus, RepoMapEdge,
    RepoMapEdgeKind, RepoMapLite, RepoMapLiteOptions, RepoSourceFileRef, RepoSymbolKind,
    RepoSymbolRef, build_repo_map_lite,
};
pub use service::{
    CodeActionSummary, CodeDiagnostic, CodeEditPlan, CodeIntelResponse, CodeIntelServerStatus,
    CodeIntelStatus, CodeIntelligenceService, CodeLocation, CodeRange, CodeSymbol, QueryMetadata,
};
pub use tools::register_code_intelligence_tools;

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
