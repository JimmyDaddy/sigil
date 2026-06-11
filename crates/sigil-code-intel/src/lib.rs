pub mod cache;
pub mod discovery;
pub mod error;
pub mod language;
pub mod lsp;
pub mod service;
pub mod tools;
pub mod workspace;

pub use service::{
    CodeDiagnostic, CodeIntelResponse, CodeIntelServerStatus, CodeIntelStatus,
    CodeIntelligenceService, CodeLocation, CodeRange, CodeSymbol, QueryMetadata,
};
pub use tools::register_code_intelligence_tools;

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
