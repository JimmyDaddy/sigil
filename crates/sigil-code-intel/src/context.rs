use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sigil_kernel::{
    ContextBodyRef, ContextEgressDecisionId, ContextInclusionReason, ContextItem, ContextItemId,
    ContextRepoRevision, ContextSensitivity, ContextSource, ContextTrustLevel, EventId,
    estimate_context_token_cost,
};

use crate::language::rust_document_symbols;
use crate::service::{CodeDiagnostic, CodeLocation, CodeRange, CodeSymbol};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct CodeContextHit {
    pub item: ContextItem,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<CodeRange>,
    pub snippet: String,
}

/// Cached LSP context made available to prompt assembly without starting or querying LSP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspContextSnapshot {
    pub status: LspContextSnapshotStatus,
    pub symbols: Vec<CodeSymbol>,
    pub diagnostics: Vec<CodeDiagnostic>,
    pub references: Vec<CodeLocation>,
}

impl Default for LspContextSnapshot {
    fn default() -> Self {
        Self::ready()
    }
}

impl LspContextSnapshot {
    #[must_use]
    pub fn ready() -> Self {
        Self {
            status: LspContextSnapshotStatus::Ready,
            symbols: Vec::new(),
            diagnostics: Vec::new(),
            references: Vec::new(),
        }
    }

    #[must_use]
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            status: LspContextSnapshotStatus::Unavailable {
                reason: reason.into(),
            },
            symbols: Vec::new(),
            diagnostics: Vec::new(),
            references: Vec::new(),
        }
    }

    #[must_use]
    pub fn timed_out(timeout_ms: u64) -> Self {
        Self {
            status: LspContextSnapshotStatus::TimedOut { timeout_ms },
            symbols: Vec::new(),
            diagnostics: Vec::new(),
            references: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_symbols(mut self, symbols: Vec<CodeSymbol>) -> Self {
        self.symbols = symbols;
        self
    }

    #[must_use]
    pub fn with_diagnostics(mut self, diagnostics: Vec<CodeDiagnostic>) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    #[must_use]
    pub fn with_references(mut self, references: Vec<CodeLocation>) -> Self {
        self.references = references;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspContextSnapshotStatus {
    Ready,
    Unavailable { reason: String },
    TimedOut { timeout_ms: u64 },
}

/// Bounded, request-local repository source map for Context V0 candidate selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoMapLite {
    pub repo_revision: Option<ContextRepoRevision>,
    pub files_scanned: usize,
    pub symbols: Vec<RepoSymbolRef>,
    pub source_files: Vec<RepoSourceFileRef>,
    pub edges: Vec<RepoMapEdge>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSourceFileRef {
    pub path: PathBuf,
    pub language: String,
    pub token_cost_hint: usize,
    pub indexed_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSymbolRef {
    pub symbol_id: String,
    pub name: String,
    pub kind: RepoSymbolKind,
    pub path: PathBuf,
    pub range: Option<CodeRange>,
    pub visibility: Option<String>,
    pub token_cost_hint: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoSymbolKind {
    Function,
    Struct,
    Enum,
    Trait,
    Type,
    Const,
    Static,
    Module,
    Impl,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoMapEdge {
    pub from: String,
    pub to: String,
    pub kind: RepoMapEdgeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoMapEdgeKind {
    SameFile,
    SameModule,
    DeclaredIn,
    Imports,
    References,
    TestTarget,
    RecentlyChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepoMapLiteOptions {
    pub max_files_scanned: usize,
    pub max_index_bytes_per_file: usize,
}

impl Default for RepoMapLiteOptions {
    fn default() -> Self {
        Self {
            max_files_scanned: 640,
            max_index_bytes_per_file: 192 * 1024,
        }
    }
}

/// Builds an in-memory Rust source map for one Context V0 collection pass.
///
/// This intentionally does not persist a repo graph. It scans bounded source roots, skips generated
/// and local-development directories, and keeps indexed file text capped for request-local ranking.
///
/// # Errors
///
/// Returns an error when the workspace root cannot be canonicalized.
pub fn build_repo_map_lite(
    workspace_root: &Path,
    options: RepoMapLiteOptions,
) -> Result<RepoMapLite> {
    let workspace_root = workspace_root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", workspace_root.display()))?;
    let max_files_scanned = options.max_files_scanned.max(1);
    let max_index_bytes = options.max_index_bytes_per_file.max(1);
    let mut map = RepoMapLite {
        repo_revision: None,
        files_scanned: 0,
        symbols: Vec::new(),
        source_files: Vec::new(),
        edges: Vec::new(),
    };

    let mut stack = repo_map_scan_roots(&workspace_root);
    while let Some(path) = stack.pop() {
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        let mut entries = entries.flatten().collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if !should_skip_repo_map_dir(&path) {
                    stack.push(path);
                }
                continue;
            }
            if !file_type.is_file() || map.files_scanned >= max_files_scanned {
                continue;
            }
            let Ok(relative) = path.strip_prefix(&workspace_root) else {
                continue;
            };
            if should_skip_repo_map_path(relative) {
                continue;
            }
            map.files_scanned = map.files_scanned.saturating_add(1);
            if is_secret_like_repo_map_path(relative)
                || relative
                    .extension()
                    .and_then(|extension| extension.to_str())
                    != Some("rs")
            {
                continue;
            }
            let Some(indexed_text) = read_repo_map_index(&path, max_index_bytes) else {
                continue;
            };
            let relative = relative.to_path_buf();
            map.source_files.push(RepoSourceFileRef {
                path: relative.clone(),
                language: "rust".to_owned(),
                token_cost_hint: estimate_context_token_cost(&indexed_text),
                indexed_text,
            });
            let symbols = rust_document_symbols(&workspace_root, &path, None, 128)
                .unwrap_or_else(|_| Vec::new());
            for symbol in symbols {
                let symbol_ref = repo_symbol_ref(symbol);
                map.edges.push(RepoMapEdge {
                    from: symbol_ref.symbol_id.clone(),
                    to: format!("file:{}", symbol_ref.path.display()),
                    kind: RepoMapEdgeKind::DeclaredIn,
                });
                map.symbols.push(symbol_ref);
            }
        }
        if map.files_scanned >= max_files_scanned {
            break;
        }
    }

    map.source_files
        .sort_by(|left, right| left.path.cmp(&right.path));
    map.symbols.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.symbol_id.cmp(&right.symbol_id))
    });
    map.edges.sort_by(|left, right| {
        left.from
            .cmp(&right.from)
            .then_with(|| left.to.cmp(&right.to))
    });
    Ok(map)
}

#[derive(Debug, Clone)]
pub struct CodeContextBuilder {
    trust_level: ContextTrustLevel,
    sensitivity: ContextSensitivity,
    egress_decision: Option<ContextEgressDecisionId>,
    source_event_id: Option<EventId>,
}

fn repo_symbol_ref(symbol: CodeSymbol) -> RepoSymbolRef {
    let path = PathBuf::from(symbol.path);
    let kind = repo_symbol_kind(&symbol.kind);
    let token_cost_hint = estimate_context_token_cost(&format!("{} {}", symbol.kind, symbol.name));
    let symbol_id = format!("symbol:{}:{}", path.display(), symbol.name);
    RepoSymbolRef {
        symbol_id,
        name: symbol.name,
        kind,
        path,
        range: Some(symbol.range),
        visibility: None,
        token_cost_hint,
    }
}

fn repo_symbol_kind(kind: &str) -> RepoSymbolKind {
    match kind {
        "function" => RepoSymbolKind::Function,
        "struct" => RepoSymbolKind::Struct,
        "enum" => RepoSymbolKind::Enum,
        "trait" => RepoSymbolKind::Trait,
        "type" => RepoSymbolKind::Type,
        "const" => RepoSymbolKind::Const,
        "static" => RepoSymbolKind::Static,
        "module" => RepoSymbolKind::Module,
        "impl" => RepoSymbolKind::Impl,
        _ => RepoSymbolKind::Other,
    }
}

fn repo_map_scan_roots(workspace_root: &Path) -> Vec<PathBuf> {
    let crates_root = workspace_root.join("crates");
    if crates_root.is_dir() {
        vec![crates_root]
    } else {
        vec![workspace_root.to_path_buf()]
    }
}

fn read_repo_map_index(path: &Path, max_bytes: usize) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let indexed_len = bytes.len().min(max_bytes);
    let indexed = &bytes[..indexed_len];
    if indexed.contains(&0) {
        return None;
    }
    std::str::from_utf8(indexed).ok().map(str::to_owned)
}

fn should_skip_repo_map_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_skipped_repo_map_component)
}

fn should_skip_repo_map_path(relative: &Path) -> bool {
    relative.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(is_skipped_repo_map_component)
    })
}

fn is_skipped_repo_map_component(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".sigil"
            | ".repo-local-dev"
            | "target"
            | "node_modules"
            | "dist"
            | "build"
            | "coverage"
            | ".pytest_cache"
            | "__pycache__"
    )
}

fn is_secret_like_repo_map_path(relative: &Path) -> bool {
    let text = relative.to_string_lossy().to_lowercase();
    text.ends_with(".env")
        || text.contains("/.env")
        || text.contains("id_rsa")
        || text.contains("id_ed25519")
        || text.contains("private_key")
        || text.ends_with(".pem")
        || text.ends_with(".key")
}

impl Default for CodeContextBuilder {
    fn default() -> Self {
        Self {
            trust_level: ContextTrustLevel::UntrustedRepositoryData,
            sensitivity: ContextSensitivity::Repository,
            egress_decision: None,
            source_event_id: None,
        }
    }
}

#[cfg(test)]
#[path = "tests/context_tests.rs"]
mod tests;

impl CodeContextBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn trust_level(mut self, trust_level: ContextTrustLevel) -> Self {
        self.trust_level = trust_level;
        self
    }

    #[must_use]
    pub fn sensitivity(mut self, sensitivity: ContextSensitivity) -> Self {
        self.sensitivity = sensitivity;
        self
    }

    #[must_use]
    pub fn egress_decision(mut self, egress_decision: impl Into<ContextEgressDecisionId>) -> Self {
        self.egress_decision = Some(egress_decision.into());
        self
    }

    #[must_use]
    pub fn source_event_id(mut self, source_event_id: impl Into<EventId>) -> Self {
        self.source_event_id = Some(source_event_id.into());
        self
    }

    #[must_use]
    pub fn symbol_hit(&self, symbol: &CodeSymbol) -> CodeContextHit {
        let snippet = format!(
            "{} {} at {}:{}",
            symbol.kind, symbol.name, symbol.path, symbol.range.start_line
        );
        self.hit(
            format!("lsp-symbol:{}:{}", symbol.path, symbol.name),
            ContextSource::LspSymbol,
            PathBuf::from(&symbol.path),
            Some(symbol.range.clone()),
            snippet,
        )
    }

    #[must_use]
    pub fn diagnostic_hit(&self, diagnostic: &CodeDiagnostic) -> CodeContextHit {
        let snippet = format!(
            "{} diagnostic at {}:{}: {}",
            diagnostic.severity, diagnostic.path, diagnostic.range.start_line, diagnostic.message
        );
        self.hit(
            format!(
                "lsp-diagnostic:{}:{}:{}",
                diagnostic.path, diagnostic.range.start_line, diagnostic.message
            ),
            ContextSource::LspDiagnostic,
            PathBuf::from(&diagnostic.path),
            Some(diagnostic.range.clone()),
            snippet,
        )
    }

    #[must_use]
    pub fn reference_hit(&self, location: &CodeLocation) -> CodeContextHit {
        let preview = location.preview.as_deref().unwrap_or("reference");
        let snippet = format!(
            "reference at {}:{}: {}",
            location.path, location.range.start_line, preview
        );
        self.hit(
            format!(
                "lsp-reference:{}:{}",
                location.path, location.range.start_line
            ),
            ContextSource::LspReference,
            PathBuf::from(&location.path),
            Some(location.range.clone()),
            snippet,
        )
    }

    #[must_use]
    pub fn repo_file_hit(
        &self,
        path: impl Into<PathBuf>,
        body: impl Into<String>,
    ) -> CodeContextHit {
        let path = path.into();
        let snippet = body.into();
        self.hit(
            format!("repo-file:{}", path.display()),
            ContextSource::RepositoryFile,
            path,
            None,
            snippet,
        )
    }

    #[must_use]
    pub fn current_diff_hit(
        &self,
        path: impl Into<PathBuf>,
        diff: impl Into<String>,
    ) -> CodeContextHit {
        let path = path.into();
        let snippet = diff.into();
        self.hit(
            format!("current-diff:{}", path.display()),
            ContextSource::CurrentDiff,
            path,
            None,
            snippet,
        )
    }

    fn hit(
        &self,
        id: ContextItemId,
        source: ContextSource,
        path: PathBuf,
        range: Option<CodeRange>,
        snippet: String,
    ) -> CodeContextHit {
        let inclusion_reason =
            if self.sensitivity == ContextSensitivity::Secret && self.egress_decision.is_none() {
                ContextInclusionReason::ExcludedSecret
            } else {
                ContextInclusionReason::RetrievalHit
            };
        let item = ContextItem {
            id,
            source,
            source_event_id: self.source_event_id.clone(),
            trust_level: self.trust_level,
            sensitivity: self.sensitivity,
            egress_decision: self.egress_decision.clone(),
            repo_revision: None,
            token_cost: estimate_context_token_cost(&snippet),
            score: None,
            score_breakdown: Vec::new(),
            inclusion_reason,
            body_ref: ContextBodyRef::inline(&snippet),
        };
        CodeContextHit {
            item,
            path,
            range,
            snippet,
        }
    }
}
