use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use sigil_kernel::{
    ContextBodyRef, ContextEgressDecisionId, ContextInclusionReason, ContextItem, ContextItemId,
    ContextRepoRevision, ContextSensitivity, ContextSource, ContextTrustLevel, EventId,
    estimate_context_token_cost,
};

use crate::repo_language::{
    RepoDefinitionTag, RepoLanguage, extract_repo_tags, repo_language_for_path,
};
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
    pub entries_walked: usize,
    pub files_scanned: usize,
    pub symbols: Vec<RepoSymbolRef>,
    pub references: Vec<RepoReferenceRef>,
    pub source_files: Vec<RepoSourceFileRef>,
    pub edges: Vec<RepoMapEdge>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSourceFileRef {
    pub path: PathBuf,
    pub language: String,
    pub token_cost_hint: usize,
    pub indexed_text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSymbolRef {
    pub symbol_id: String,
    pub name: String,
    pub language: String,
    pub kind: RepoSymbolKind,
    pub path: PathBuf,
    pub range: Option<CodeRange>,
    pub visibility: Option<String>,
    pub token_cost_hint: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoReferenceRef {
    pub name: String,
    pub language: String,
    pub path: PathBuf,
    pub range: CodeRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RepoSymbolKind {
    Function,
    Method,
    Class,
    Interface,
    Struct,
    Enum,
    Trait,
    Type,
    Const,
    Static,
    Module,
    Impl,
    Variable,
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
    pub max_walked_entries: usize,
    pub max_source_files: usize,
    pub max_index_bytes_per_file: usize,
    pub max_definitions_per_file: usize,
    pub max_references_per_file: usize,
    pub max_definitions: usize,
    pub max_references: usize,
    pub max_edges: usize,
}

impl Default for RepoMapLiteOptions {
    fn default() -> Self {
        Self {
            max_walked_entries: 4_096,
            max_source_files: 640,
            max_index_bytes_per_file: 192 * 1024,
            max_definitions_per_file: 128,
            max_references_per_file: 256,
            max_definitions: 8_192,
            max_references: 16_384,
            max_edges: 16_384,
        }
    }
}

/// Builds an in-memory multilingual source map for one request-local context collection pass.
///
/// This intentionally does not persist a repo graph. It uses the repository's ignore rules, does
/// not follow symlinks, caps both traversal and source parsing, and emits only same-language unique
/// definition reference edges.
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
    let max_walked_entries = options.max_walked_entries.max(1);
    let max_source_files = options.max_source_files.max(1);
    let max_index_bytes = options.max_index_bytes_per_file.max(1);
    let max_definitions_per_file = options.max_definitions_per_file.max(1);
    let max_references_per_file = options.max_references_per_file.max(1);
    let max_definitions = options.max_definitions.max(1);
    let max_references = options.max_references.max(1);
    let max_edges = options.max_edges.max(1);
    let mut map = RepoMapLite {
        repo_revision: None,
        entries_walked: 0,
        files_scanned: 0,
        symbols: Vec::new(),
        references: Vec::new(),
        source_files: Vec::new(),
        edges: Vec::new(),
    };

    let filter_root = workspace_root.clone();
    let mut walker = WalkBuilder::new(&workspace_root);
    walker
        .hidden(false)
        .parents(true)
        .ignore(true)
        .git_global(true)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(false)
        .follow_links(false)
        .sort_by_file_path(|left, right| left.cmp(right))
        .filter_entry(move |entry| {
            entry
                .path()
                .strip_prefix(&filter_root)
                .map_or(true, |relative| !should_skip_repo_map_path(relative))
        });

    for result in walker.build() {
        if map.entries_walked >= max_walked_entries || map.files_scanned >= max_source_files {
            break;
        }
        map.entries_walked = map.entries_walked.saturating_add(1);
        let Ok(entry) = result else {
            continue;
        };
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }
        let path = entry.into_path();
        let Ok(relative) = path.strip_prefix(&workspace_root) else {
            continue;
        };
        if is_secret_like_repo_map_path(relative) {
            continue;
        }
        let Some(language) = repo_language_for_path(relative) else {
            continue;
        };
        map.files_scanned = map.files_scanned.saturating_add(1);
        let Some(indexed) = read_repo_map_index(&path, max_index_bytes) else {
            continue;
        };
        let relative = relative.to_path_buf();
        let tags = extract_repo_tags(
            language,
            &workspace_root,
            &path,
            &indexed.text,
            max_definitions_per_file,
            max_references_per_file,
        )
        .with_context(|| format!("failed to extract RepoMap tags from {}", relative.display()))?;
        map.source_files.push(RepoSourceFileRef {
            path: relative.clone(),
            language: language.as_str().to_owned(),
            token_cost_hint: estimate_context_token_cost(&indexed.text),
            indexed_text: indexed.text,
            truncated: indexed.truncated,
        });

        for definition in tags.definitions {
            if map.symbols.len() >= max_definitions {
                break;
            }
            let symbol_ref = repo_symbol_ref(language, &relative, definition);
            if map.edges.len() < max_edges {
                map.edges.push(RepoMapEdge {
                    from: symbol_ref.symbol_id.clone(),
                    to: format!("file:{}", symbol_ref.path.display()),
                    kind: RepoMapEdgeKind::DeclaredIn,
                });
            }
            map.symbols.push(symbol_ref);
        }
        for reference in tags.references {
            if map.references.len() >= max_references {
                break;
            }
            map.references.push(RepoReferenceRef {
                name: reference.name,
                language: language.as_str().to_owned(),
                path: relative.clone(),
                range: reference.range,
            });
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
    map.references.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| code_range_key(&left.range).cmp(&code_range_key(&right.range)))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.language.cmp(&right.language))
    });
    append_unique_reference_edges(&mut map, max_edges);
    map.edges.sort_by(|left, right| {
        left.from
            .cmp(&right.from)
            .then_with(|| left.to.cmp(&right.to))
            .then_with(|| repo_edge_kind_rank(left.kind).cmp(&repo_edge_kind_rank(right.kind)))
    });
    map.edges.dedup();
    Ok(map)
}

#[derive(Debug, Clone)]
pub struct CodeContextBuilder {
    trust_level: ContextTrustLevel,
    sensitivity: ContextSensitivity,
    egress_decision: Option<ContextEgressDecisionId>,
    source_event_id: Option<EventId>,
}

fn repo_symbol_ref(
    language: RepoLanguage,
    path: &Path,
    definition: RepoDefinitionTag,
) -> RepoSymbolRef {
    let token_cost_hint =
        estimate_context_token_cost(&format!("{:?} {}", definition.kind, definition.name));
    let symbol_id = format!(
        "symbol:{}:{}:{}:{}:{}",
        language.as_str(),
        path.display(),
        definition.name,
        definition.range.start_line,
        definition.range.start_character
    );
    RepoSymbolRef {
        symbol_id,
        name: definition.name,
        language: language.as_str().to_owned(),
        kind: definition.kind,
        path: path.to_path_buf(),
        range: Some(definition.range),
        visibility: None,
        token_cost_hint,
    }
}

fn append_unique_reference_edges(map: &mut RepoMapLite, max_edges: usize) {
    let mut definitions = BTreeMap::<(String, String), Vec<String>>::new();
    for symbol in &map.symbols {
        definitions
            .entry((symbol.language.clone(), symbol.name.clone()))
            .or_default()
            .push(symbol.symbol_id.clone());
    }
    for reference in &map.references {
        if map.edges.len() >= max_edges {
            break;
        }
        let Some(targets) = definitions.get(&(reference.language.clone(), reference.name.clone()))
        else {
            continue;
        };
        if let [target] = targets.as_slice() {
            map.edges.push(RepoMapEdge {
                from: format!("file:{}", reference.path.display()),
                to: target.clone(),
                kind: RepoMapEdgeKind::References,
            });
        }
    }
}

struct RepoMapIndex {
    text: String,
    truncated: bool,
}

fn read_repo_map_index(path: &Path, max_bytes: usize) -> Option<RepoMapIndex> {
    let file = File::open(path).ok()?;
    let read_limit = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX - 1)
        .saturating_add(1);
    let mut bytes = Vec::with_capacity(max_bytes.saturating_add(1));
    file.take(read_limit).read_to_end(&mut bytes).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    let truncated = bytes.len() > max_bytes;
    if truncated {
        bytes.truncate(max_bytes);
    }
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text.to_owned(),
        Err(error) if error.error_len().is_none() => {
            bytes.truncate(error.valid_up_to());
            std::str::from_utf8(&bytes).ok()?.to_owned()
        }
        Err(_) => return None,
    };
    Some(RepoMapIndex { text, truncated })
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
            | "vendor"
            | "out"
            | ".next"
            | ".cache"
            | ".mypy_cache"
            | ".pytest_cache"
            | ".venv"
            | "venv"
            | "__pycache__"
    )
}

fn is_secret_like_repo_map_path(relative: &Path) -> bool {
    let text = relative.to_string_lossy().to_lowercase();
    let file_name = relative
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    file_name.starts_with(".env")
        || text.contains("id_rsa")
        || text.contains("id_ed25519")
        || text.contains("private_key")
        || text.ends_with(".pem")
        || text.ends_with(".key")
}

fn code_range_key(range: &CodeRange) -> (u64, u64, u64, u64) {
    (
        range.start_line,
        range.start_character,
        range.end_line,
        range.end_character,
    )
}

const fn repo_edge_kind_rank(kind: RepoMapEdgeKind) -> u8 {
    match kind {
        RepoMapEdgeKind::SameFile => 0,
        RepoMapEdgeKind::SameModule => 1,
        RepoMapEdgeKind::DeclaredIn => 2,
        RepoMapEdgeKind::Imports => 3,
        RepoMapEdgeKind::References => 4,
        RepoMapEdgeKind::TestTarget => 5,
        RepoMapEdgeKind::RecentlyChanged => 6,
    }
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
