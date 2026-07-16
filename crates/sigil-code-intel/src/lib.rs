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
pub use service::{
    CodeActionSummary, CodeDiagnostic, CodeEditPlan, CodeIntelResponse, CodeIntelServerStatus,
    CodeIntelStatus, CodeIntelligenceService, CodeLocation, CodeRange, CodeSymbol, QueryMetadata,
};
pub use tools::{
    register_code_intelligence_tools, register_code_intelligence_tools_with_workspace_trust,
};
pub use workspace::{
    EffectiveServerPlan, PlannedServerStatus, config_enabled, effective_server_plan,
};

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
