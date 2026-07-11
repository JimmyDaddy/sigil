use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sigil_kernel::{
    CodeIntelStartup, CodeIntelligenceConfig, LanguageServerConfig, WorkspaceTrust,
};
use tokio::{
    io::AsyncReadExt,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};

use crate::{
    cache::TimedCache,
    edit::{CodeWorkspaceEdit, workspace_edit_from_lsp},
    error::CodeIntelError,
    language::{rust_document_symbols, rust_syntax_diagnostics},
    lsp::{
        LspClient, code_action_resolve_supported, code_action_supported, definition_supported,
        diagnostics_supported, document_symbol_supported, lsp_error_to_reason,
        lsp_uri_to_workspace_path, position_params, references_supported, rename_supported,
        response_array, text_document_identifier, workspace_symbol_supported,
    },
    workspace::{
        EffectiveServerPlan, config_enabled, effective_server_plan, file_uri_from_path,
        find_server_root, language_for_path, resolve_workspace_file, safe_lsp_command,
        sanitize_lsp_env, server_for_path, workspace_relative_path,
    },
};

mod parsers; // LSP value parsing into model-facing code-intel DTOs.
mod preview; // source-line preview extraction.
mod requests; // public service facade and query orchestration.
mod server; // language server process lifecycle and synchronization.
mod status; // response envelopes, metadata, and server status rows.

use parsers::{
    code_action_params, collect_lsp_symbols, is_rust_source_path, lsp_symbol_kind,
    parse_code_action_summary, parse_diagnostic_value, pull_diagnostics_from_response,
    select_code_action,
};
use preview::preview_line;
use server::{
    LanguageServerHandle, LspRequestOutput, ProcessLanguageServer, ServerPlanState, drain_stderr,
    initial_server_plan, language_server_mut, server_plan_state_from_effective,
};
use status::{response, response_with_filtered, response_with_statuses, server_status};

#[cfg(test)]
use parsers::lsp_diagnostic_severity;

pub(crate) use parsers::parse_range;
pub use parsers::{CodeActionSummary, CodeDiagnostic, CodeLocation, CodeRange, CodeSymbol};
pub use requests::{CodeEditPlan, CodeIntelligenceService};
pub use status::{CodeIntelResponse, CodeIntelServerStatus, CodeIntelStatus, QueryMetadata};

#[cfg(test)]
#[path = "../tests/service_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/trust_tests.rs"]
mod trust_tests;
