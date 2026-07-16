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

/// One bounded code-intelligence result projected into Sigil's context contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct CodeContextHit {
    /// Provider-neutral context item with trust, sensitivity, source, and token metadata.
    pub item: ContextItem,
    /// Workspace-relative path associated with the result.
    pub path: PathBuf,
    /// Source range when the underlying result identifies one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<CodeRange>,
    /// Bounded human-readable snippet selected for prompt projection.
    pub snippet: String,
}

/// Cached LSP context made available to prompt assembly without starting or querying LSP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspContextSnapshot {
    /// Whether the warm cache was ready, unavailable, or exceeded its read deadline.
    pub status: LspContextSnapshotStatus,
    /// Bounded symbols already present in the warm LSP cache.
    pub symbols: Vec<CodeSymbol>,
    /// Bounded diagnostics already present in the warm LSP cache.
    pub diagnostics: Vec<CodeDiagnostic>,
    /// Bounded reference locations already present in the warm LSP cache.
    pub references: Vec<CodeLocation>,
}

impl Default for LspContextSnapshot {
    fn default() -> Self {
        Self::ready()
    }
}

impl LspContextSnapshot {
    /// Creates an empty snapshot whose cache read completed successfully.
    #[must_use]
    pub fn ready() -> Self {
        Self {
            status: LspContextSnapshotStatus::Ready,
            symbols: Vec::new(),
            diagnostics: Vec::new(),
            references: Vec::new(),
        }
    }

    /// Creates a snapshot that records why the warm cache could not be used.
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

    /// Creates a snapshot that records the bounded cache-read timeout.
    #[must_use]
    pub fn timed_out(timeout_ms: u64) -> Self {
        Self {
            status: LspContextSnapshotStatus::TimedOut { timeout_ms },
            symbols: Vec::new(),
            diagnostics: Vec::new(),
            references: Vec::new(),
        }
    }

    /// Replaces the snapshot's bounded symbol rows.
    #[must_use]
    pub fn with_symbols(mut self, symbols: Vec<CodeSymbol>) -> Self {
        self.symbols = symbols;
        self
    }

    /// Replaces the snapshot's bounded diagnostic rows.
    #[must_use]
    pub fn with_diagnostics(mut self, diagnostics: Vec<CodeDiagnostic>) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    /// Replaces the snapshot's bounded reference rows.
    #[must_use]
    pub fn with_references(mut self, references: Vec<CodeLocation>) -> Self {
        self.references = references;
        self
    }
}

/// Outcome of reading the existing warm LSP cache for prompt assembly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspContextSnapshotStatus {
    /// The cache read completed and the rows in the snapshot are usable.
    Ready,
    /// The shared service or cache was unavailable without blocking fallback context collection.
    Unavailable {
        /// Safe diagnostic reason for the unavailable cache.
        reason: String,
    },
    /// The warm cache did not respond within the request-local deadline.
    TimedOut {
        /// Deadline in milliseconds used for the bounded read.
        timeout_ms: u64,
    },
}

/// Bounded, request-local repository source map for Context V1 candidate selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoMapLite {
    /// Best-effort revision identity for the scanned workspace.
    pub repo_revision: Option<ContextRepoRevision>,
    /// Number of directory entries visited before ignore and traversal caps completed.
    pub entries_walked: usize,
    /// Number of source files parsed into the request-local map.
    pub files_scanned: usize,
    /// Bounded symbol definitions extracted from supported languages.
    pub symbols: Vec<RepoSymbolRef>,
    /// Bounded lexical reference candidates extracted from supported languages.
    pub references: Vec<RepoReferenceRef>,
    /// Bounded source rows retained for request-local scoring and snippets.
    pub source_files: Vec<RepoSourceFileRef>,
    /// Heuristic, non-authoritative relationships between map rows.
    pub edges: Vec<RepoMapEdge>,
}

/// One bounded source file retained by the request-local repository map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSourceFileRef {
    /// Workspace-relative path.
    pub path: PathBuf,
    /// Normalized language identifier used by the parser adapter.
    pub language: String,
    /// Conservative token-cost hint for scheduling the row into context.
    pub token_cost_hint: usize,
    /// Bounded source text used only for local ranking and snippet selection.
    pub indexed_text: String,
    /// Whether the source text was cut at the configured per-file byte cap.
    pub truncated: bool,
}

/// One symbol definition extracted into the request-local repository map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSymbolRef {
    /// Stable request-local symbol identifier.
    pub symbol_id: String,
    /// Source-level symbol name.
    pub name: String,
    /// Normalized parser language.
    pub language: String,
    /// Provider-neutral symbol category.
    pub kind: RepoSymbolKind,
    /// Workspace-relative source path.
    pub path: PathBuf,
    /// Source range when the parser can determine it.
    pub range: Option<CodeRange>,
    /// Source-language visibility label when available.
    pub visibility: Option<String>,
    /// Conservative token-cost hint for context scheduling.
    pub token_cost_hint: usize,
}

/// One lexical reference candidate extracted into the request-local repository map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoReferenceRef {
    /// Referenced source-level name.
    pub name: String,
    /// Normalized parser language.
    pub language: String,
    /// Workspace-relative source path.
    pub path: PathBuf,
    /// Exact source range of the reference candidate.
    pub range: CodeRange,
}

/// Provider-neutral category for a repository-map symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RepoSymbolKind {
    /// Free function.
    Function,
    /// Method associated with a type or receiver.
    Method,
    /// Class declaration.
    Class,
    /// Interface declaration.
    Interface,
    /// Struct declaration.
    Struct,
    /// Enum declaration.
    Enum,
    /// Trait declaration.
    Trait,
    /// Type alias or language-level type declaration.
    Type,
    /// Constant declaration.
    Const,
    /// Static declaration.
    Static,
    /// Module or namespace declaration.
    Module,
    /// Implementation block.
    Impl,
    /// Variable declaration.
    Variable,
    /// Symbol not represented by a more specific category.
    Other,
}

/// One heuristic relationship emitted by the request-local repository map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoMapEdge {
    /// Source row identifier.
    pub from: String,
    /// Target row identifier.
    pub to: String,
    /// Heuristic relationship category.
    pub kind: RepoMapEdgeKind,
}

/// Heuristic relationship categories; none of these claims a resolved call graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoMapEdgeKind {
    /// Rows occur in the same source file.
    SameFile,
    /// Rows occur in the same source module.
    SameModule,
    /// A row is declared by the target row or scope.
    DeclaredIn,
    /// A source import mentions the target.
    Imports,
    /// A bounded lexical reference points at a unique same-language definition.
    References,
    /// A test source is associated with the target source.
    TestTarget,
    /// A row is associated with a recently changed source.
    RecentlyChanged,
}

/// Hard caps for one request-local repository-map build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepoMapLiteOptions {
    /// Maximum directory entries visited before stopping traversal.
    pub max_walked_entries: usize,
    /// Maximum source files retained for parsing and ranking.
    pub max_source_files: usize,
    /// Maximum source bytes indexed from one file.
    pub max_index_bytes_per_file: usize,
    /// Maximum definitions retained from one file.
    pub max_definitions_per_file: usize,
    /// Maximum reference candidates retained from one file.
    pub max_references_per_file: usize,
    /// Maximum definitions retained across the map.
    pub max_definitions: usize,
    /// Maximum reference candidates retained across the map.
    pub max_references: usize,
    /// Maximum heuristic edges retained across the map.
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

/// Builder that converts code-intelligence rows into trust-labeled context items.
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
    /// Creates a builder with local-generated trust and normal sensitivity defaults.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the trust label applied to subsequently projected context items.
    #[must_use]
    pub fn trust_level(mut self, trust_level: ContextTrustLevel) -> Self {
        self.trust_level = trust_level;
        self
    }

    /// Sets the sensitivity label applied to subsequently projected context items.
    #[must_use]
    pub fn sensitivity(mut self, sensitivity: ContextSensitivity) -> Self {
        self.sensitivity = sensitivity;
        self
    }

    /// Binds projected context items to an explicit egress decision.
    #[must_use]
    pub fn egress_decision(mut self, egress_decision: impl Into<ContextEgressDecisionId>) -> Self {
        self.egress_decision = Some(egress_decision.into());
        self
    }

    /// Binds projected context items to their durable source event when one exists.
    #[must_use]
    pub fn source_event_id(mut self, source_event_id: impl Into<EventId>) -> Self {
        self.source_event_id = Some(source_event_id.into());
        self
    }

    /// Projects one symbol row into a bounded context hit.
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

    /// Projects one diagnostic row into a bounded context hit.
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

    /// Projects one reference location into a bounded context hit.
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

    /// Projects bounded repository-file text into a context hit.
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

    /// Projects a bounded current-diff fragment into a context hit.
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
