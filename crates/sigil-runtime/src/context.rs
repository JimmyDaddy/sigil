use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result};
use sigil_code_intel::context::{CodeContextBuilder, CodeContextHit};
use sigil_kernel::{
    ContextInclusionReason, ContextItem, ContextSensitivity, PluginHookContextItems,
    PluginHookContextOptions, PluginHookOutputEnvelope, RuntimeContextCandidates, TaskMemoryV1,
    plugin_hook_output_context_items, task_memory_context_items,
};

const REPO_CONTEXT_MAX_FILES_SCANNED: usize = 160;
const REPO_CONTEXT_MAX_ITEMS: usize = 3;
const REPO_CONTEXT_MAX_BYTES_PER_FILE: usize = 8 * 1024;
const SOURCE_CONTEXT_MAX_FILES_SCANNED: usize = 640;
const SOURCE_CONTEXT_MAX_INDEX_BYTES_PER_FILE: usize = 192 * 1024;
const REPO_CONTEXT_SNIPPET_MAX_BYTES: usize = 2 * 1024;

#[derive(Debug, Clone)]
struct RepoContextCandidate {
    score: f32,
    inclusion_reason: ContextInclusionReason,
    snippet_terms: BTreeSet<String>,
}

/// Converts typed task memory into runtime context candidates with provenance preserved.
///
/// This is intentionally a thin runtime boundary over the kernel adapter: runtime callers can
/// assemble context without depending on TUI-specific rendering, while the trust/sensitivity
/// invariants remain enforced by kernel types.
pub fn context_items_from_task_memory(memory: &TaskMemoryV1) -> Result<Vec<ContextItem>> {
    task_memory_context_items(memory)
}

/// Converts trusted plugin hook output into runtime context candidates with provenance preserved.
///
/// Runtime callers must still decide when to execute hooks and whether to pass the resulting items
/// into prompt assembly. The helper never creates verification evidence or task-memory facts.
pub fn context_items_from_plugin_hook_output(
    output: &PluginHookOutputEnvelope,
    options: PluginHookContextOptions,
) -> Result<PluginHookContextItems> {
    plugin_hook_output_context_items(output, options)
}

/// Builds bounded repository-file Context V0 candidates from a user query.
///
/// This is intentionally conservative: it never leaves the workspace root, avoids common generated
/// and local-development directories, and emits excluded metadata instead of reading secret-like
/// files. It is a production wiring point for Context V0, not a persistent repo index.
///
/// # Errors
///
/// Returns an error when the workspace root cannot be canonicalized. Individual file read failures
/// are ignored so request assembly degrades gracefully.
pub fn context_candidates_from_repo_query(
    workspace_root: &Path,
    query: &str,
) -> Result<RuntimeContextCandidates> {
    let workspace_root = workspace_root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", workspace_root.display()))?;
    let query = query.trim();
    if query.is_empty() {
        return Ok(RuntimeContextCandidates::default());
    }

    let mut candidates = BTreeMap::<PathBuf, RepoContextCandidate>::new();
    for path in explicit_query_paths(query) {
        if let Some(relative) = normalize_workspace_relative_path(&workspace_root, &path) {
            upsert_context_candidate(
                &mut candidates,
                relative,
                100.0,
                ContextInclusionReason::RetrievalHit,
                BTreeSet::new(),
            );
        }
    }

    let explicit_candidate_count = candidates.len();
    let terms = lexical_query_terms(query);
    if explicit_candidate_count == 0 {
        if !terms.is_empty() {
            collect_lexical_file_candidates(&workspace_root, &terms, &mut candidates);
        }
        if !terms.is_empty()
            || query_has_source_intent(query)
            || !explicit_code_query_terms(query).is_empty()
        {
            collect_source_symbol_candidates(&workspace_root, query, &terms, &mut candidates);
        }
    }

    let builder = CodeContextBuilder::new();
    let mut runtime = RuntimeContextCandidates::default();
    let mut ranked = candidates.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .score
            .partial_cmp(&left.1.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    for (relative_path, candidate) in ranked.into_iter().take(REPO_CONTEXT_MAX_ITEMS) {
        let hit = repo_context_hit(&workspace_root, &builder, relative_path, candidate);
        runtime
            .snippets
            .insert(hit.item.id.clone(), hit.snippet.clone());
        runtime.items.push(hit.item);
    }
    Ok(runtime)
}

fn collect_lexical_file_candidates(
    workspace_root: &Path,
    terms: &BTreeSet<String>,
    candidates: &mut BTreeMap<PathBuf, RepoContextCandidate>,
) {
    let mut stack = vec![workspace_root.to_path_buf()];
    let mut scanned = 0usize;
    while let Some(path) = stack.pop() {
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if !should_skip_repo_context_dir(&path) {
                    stack.push(path);
                }
                continue;
            }
            if !file_type.is_file() || scanned >= REPO_CONTEXT_MAX_FILES_SCANNED {
                continue;
            }
            scanned = scanned.saturating_add(1);
            let Ok(relative) = path.strip_prefix(workspace_root) else {
                continue;
            };
            if should_skip_repo_context_path(relative) {
                continue;
            }
            let score = lexical_file_score(&path, relative, terms);
            if score > 0.0 {
                upsert_context_candidate(
                    candidates,
                    relative.to_path_buf(),
                    score,
                    ContextInclusionReason::RetrievalHit,
                    BTreeSet::new(),
                );
            }
        }
        if scanned >= REPO_CONTEXT_MAX_FILES_SCANNED {
            break;
        }
    }
}

fn collect_source_symbol_candidates(
    workspace_root: &Path,
    query: &str,
    lexical_terms: &BTreeSet<String>,
    candidates: &mut BTreeMap<PathBuf, RepoContextCandidate>,
) {
    let profile = SourceQueryProfile::from_query(query, lexical_terms);
    if !profile.should_scan_sources() {
        return;
    }

    let mut stack = source_scan_roots(workspace_root);
    let mut scanned = 0usize;
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
                if !should_skip_repo_context_dir(&path) {
                    stack.push(path);
                }
                continue;
            }
            if !file_type.is_file() || scanned >= SOURCE_CONTEXT_MAX_FILES_SCANNED {
                continue;
            }
            scanned = scanned.saturating_add(1);
            let Ok(relative) = path.strip_prefix(workspace_root) else {
                continue;
            };
            if should_skip_repo_context_path(relative)
                || relative
                    .extension()
                    .and_then(|extension| extension.to_str())
                    != Some("rs")
            {
                continue;
            }
            let Some(scored) = source_symbol_file_score(&path, relative, &profile) else {
                continue;
            };
            upsert_context_candidate(
                candidates,
                relative.to_path_buf(),
                scored.score,
                scored.inclusion_reason,
                scored.snippet_terms,
            );
        }
        if scanned >= SOURCE_CONTEXT_MAX_FILES_SCANNED {
            break;
        }
    }
}

fn upsert_context_candidate(
    candidates: &mut BTreeMap<PathBuf, RepoContextCandidate>,
    path: PathBuf,
    score: f32,
    inclusion_reason: ContextInclusionReason,
    snippet_terms: BTreeSet<String>,
) {
    candidates
        .entry(path)
        .and_modify(|existing| {
            if score > existing.score {
                existing.score = score;
                existing.inclusion_reason = inclusion_reason.clone();
                existing.snippet_terms = snippet_terms.clone();
            } else if score == existing.score && existing.snippet_terms.is_empty() {
                existing.snippet_terms = snippet_terms.clone();
            }
        })
        .or_insert(RepoContextCandidate {
            score,
            inclusion_reason,
            snippet_terms,
        });
}

fn repo_context_hit(
    workspace_root: &Path,
    builder: &CodeContextBuilder,
    relative_path: PathBuf,
    candidate: RepoContextCandidate,
) -> CodeContextHit {
    if is_secret_like_path(&relative_path) {
        let mut hit = builder
            .clone()
            .sensitivity(ContextSensitivity::Secret)
            .repo_file_hit(
                relative_path,
                "secret-like repository file omitted from automatic context",
            );
        hit.item.score = Some(candidate.score);
        return hit;
    }

    let full_path = workspace_root.join(&relative_path);
    let body =
        read_repo_context_snippet_for_candidate(&full_path, &candidate).unwrap_or_else(|| {
            format!(
                "repository file {} could not be read for automatic context",
                relative_path.display()
            )
        });
    let mut hit = builder.repo_file_hit(relative_path, body);
    hit.item.score = Some(candidate.score);
    hit.item.inclusion_reason = candidate.inclusion_reason;
    hit
}

fn read_repo_context_snippet(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let indexed_len = bytes.len().min(REPO_CONTEXT_MAX_BYTES_PER_FILE);
    let indexed = &bytes[..indexed_len];
    if indexed.contains(&0) {
        return None;
    }
    let text = std::str::from_utf8(indexed).ok()?;
    Some(truncate_to_char_boundary(text, REPO_CONTEXT_SNIPPET_MAX_BYTES).to_owned())
}

fn read_repo_context_snippet_for_candidate(
    path: &Path,
    candidate: &RepoContextCandidate,
) -> Option<String> {
    if !candidate.snippet_terms.is_empty()
        && let Some(snippet) =
            read_repo_context_snippet_around_terms(path, &candidate.snippet_terms)
    {
        return Some(snippet);
    }
    read_repo_context_snippet(path)
}

fn read_repo_context_snippet_around_terms(path: &Path, terms: &BTreeSet<String>) -> Option<String> {
    let text = read_repo_context_index(path)?;
    let lower_text = text.to_ascii_lowercase();
    let mut ranked_terms = terms.iter().collect::<Vec<_>>();
    ranked_terms.sort_by_key(|term| std::cmp::Reverse(term.len()));
    let position = ranked_terms
        .into_iter()
        .find_map(|term| lower_text.find(term.as_str()))?;
    Some(snippet_window_around_byte(&text, position, REPO_CONTEXT_SNIPPET_MAX_BYTES).to_owned())
}

fn lexical_file_score(path: &Path, relative: &Path, terms: &BTreeSet<String>) -> f32 {
    let relative_text = relative.to_string_lossy().to_lowercase();
    let mut score = terms
        .iter()
        .filter(|term| relative_text.contains(term.as_str()))
        .count() as f32
        * 10.0;

    if score == 0.0 && !looks_like_text_file(relative) {
        return 0.0;
    }

    if let Some(snippet) = read_repo_context_snippet(path) {
        let text = snippet.to_lowercase();
        score += terms
            .iter()
            .filter(|term| text.contains(term.as_str()))
            .count() as f32;
    }
    score
}

#[derive(Debug, Clone)]
struct SourceQueryProfile {
    source_intent: bool,
    lexical_terms: BTreeSet<String>,
    symbol_terms: BTreeSet<String>,
}

impl SourceQueryProfile {
    fn from_query(query: &str, lexical_terms: &BTreeSet<String>) -> Self {
        let mut source_intent = query_has_source_intent(query);
        let mut source_terms = BTreeSet::new();
        let mut symbol_terms = BTreeSet::new();

        for term in explicit_code_query_terms(query) {
            if is_path_like_query_term(&term) {
                source_terms.insert(term.to_ascii_lowercase());
            } else {
                for variant in source_term_variants(&term) {
                    symbol_terms.insert(variant.clone());
                    source_terms.insert(variant);
                }
            }
        }

        for term in lexical_terms {
            match source_query_term_role(term) {
                SourceQueryTermRole::SourceIntentHint => {
                    source_intent = true;
                }
                SourceQueryTermRole::SymbolLike => {
                    for variant in source_term_variants(term) {
                        symbol_terms.insert(variant.clone());
                        source_terms.insert(variant);
                    }
                }
                SourceQueryTermRole::PathLike | SourceQueryTermRole::LexicalHint => {
                    source_terms.insert(term.clone());
                }
                SourceQueryTermRole::NaturalLanguage => {}
            }
        }

        for token in source_query_tokens(query) {
            match source_query_term_role(&token) {
                SourceQueryTermRole::SourceIntentHint => {
                    source_intent = true;
                }
                SourceQueryTermRole::SymbolLike | SourceQueryTermRole::PathLike => {
                    for variant in source_term_variants(&token) {
                        symbol_terms.insert(variant.clone());
                        source_terms.insert(variant);
                    }
                }
                SourceQueryTermRole::LexicalHint => {
                    source_terms.insert(token.to_ascii_lowercase());
                }
                SourceQueryTermRole::NaturalLanguage => {}
            }
        }

        Self {
            source_intent,
            lexical_terms: source_terms,
            symbol_terms,
        }
    }

    fn should_scan_sources(&self) -> bool {
        self.source_intent || !self.symbol_terms.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceQueryTermRole {
    SourceIntentHint,
    SymbolLike,
    PathLike,
    LexicalHint,
    NaturalLanguage,
}

fn source_query_term_role(term: &str) -> SourceQueryTermRole {
    let trimmed = term.trim_matches(|ch: char| matches!(ch, '`' | '"' | '\''));
    if trimmed.is_empty() {
        return SourceQueryTermRole::NaturalLanguage;
    }

    let lower = trimmed.to_ascii_lowercase();
    if is_path_like_query_term(trimmed) {
        return SourceQueryTermRole::PathLike;
    }
    if is_source_intent_hint(&lower) {
        return SourceQueryTermRole::SourceIntentHint;
    }
    if is_natural_language_query_term(&lower) {
        return SourceQueryTermRole::NaturalLanguage;
    }
    if is_code_like_query_token(trimmed) {
        return SourceQueryTermRole::SymbolLike;
    }
    if lower.len() >= 4 {
        return SourceQueryTermRole::LexicalHint;
    }

    SourceQueryTermRole::NaturalLanguage
}

#[derive(Debug, Clone)]
struct SourceSymbolScore {
    score: f32,
    inclusion_reason: ContextInclusionReason,
    snippet_terms: BTreeSet<String>,
}

fn source_symbol_file_score(
    path: &Path,
    relative: &Path,
    profile: &SourceQueryProfile,
) -> Option<SourceSymbolScore> {
    let relative_text = relative.to_string_lossy().to_ascii_lowercase();
    let file_stem = relative
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let index = read_repo_context_index(path)?;
    let index_text = index.to_ascii_lowercase();
    let mut score = if profile.source_intent { 28.0 } else { 0.0 };
    let mut matched_symbol = false;
    let mut snippet_terms = BTreeSet::new();

    for term in &profile.symbol_terms {
        if file_stem == *term {
            score += 130.0;
            matched_symbol = true;
            snippet_terms.insert(term.clone());
        } else if file_stem.contains(term) {
            score += 85.0;
            matched_symbol = true;
            snippet_terms.insert(term.clone());
        }
        if relative_text.contains(term) {
            score += 70.0;
            matched_symbol = true;
            snippet_terms.insert(term.clone());
        }
        if index_text.contains(term) {
            score += 95.0;
            matched_symbol = true;
            snippet_terms.insert(term.clone());
        }
    }

    for term in &profile.lexical_terms {
        if relative_text.contains(term) {
            score += 18.0;
            snippet_terms.insert(term.clone());
        }
        if index_text.contains(term) {
            score += 4.0;
            snippet_terms.insert(term.clone());
        }
    }

    if relative.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|value| value == "tests")
    }) {
        score *= 0.45;
    }

    if score < 36.0 {
        return None;
    }

    let inclusion_reason = if matched_symbol {
        ContextInclusionReason::ExactSymbolMatch
    } else {
        ContextInclusionReason::SourcePathMatch
    };
    Some(SourceSymbolScore {
        score,
        inclusion_reason,
        snippet_terms,
    })
}

fn source_scan_roots(workspace_root: &Path) -> Vec<PathBuf> {
    let crates_root = workspace_root.join("crates");
    if crates_root.is_dir() {
        vec![crates_root]
    } else {
        vec![workspace_root.to_path_buf()]
    }
}

fn read_repo_context_index(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let indexed_len = bytes.len().min(SOURCE_CONTEXT_MAX_INDEX_BYTES_PER_FILE);
    let indexed = &bytes[..indexed_len];
    if indexed.contains(&0) {
        return None;
    }
    std::str::from_utf8(indexed).ok().map(str::to_owned)
}

fn query_has_source_intent(query: &str) -> bool {
    let lower = query.to_lowercase();
    contains_any(
        &lower,
        &[
            "rust",
            "source",
            "source file",
            "源码",
            "源码文件",
            "函数",
            "trait",
            "function",
            "definition",
            "defined",
            "module",
            "模块",
            "runner",
            "handoff",
            "surface",
            "在哪个",
            "定义在哪",
        ],
    )
}

fn source_query_tokens(query: &str) -> impl Iterator<Item = String> + '_ {
    query
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-')
        .map(str::trim)
        .filter(|token| token.len() >= 3)
        .map(str::to_owned)
}

fn explicit_code_query_terms(query: &str) -> BTreeSet<String> {
    let mut terms = BTreeSet::new();
    collect_delimited_query_terms(query, '`', &mut terms);
    collect_delimited_query_terms(query, '"', &mut terms);
    collect_delimited_query_terms(query, '\'', &mut terms);
    terms
}

fn collect_delimited_query_terms(query: &str, delimiter: char, terms: &mut BTreeSet<String>) {
    let mut rest = query;
    while let Some(start) = rest.find(delimiter) {
        rest = &rest[start + delimiter.len_utf8()..];
        let Some(end) = rest.find(delimiter) else {
            break;
        };
        collect_explicit_query_term(&rest[..end], terms);
        rest = &rest[end + delimiter.len_utf8()..];
    }
}

fn collect_explicit_query_term(segment: &str, terms: &mut BTreeSet<String>) {
    let term = segment
        .trim()
        .trim_matches(|ch: char| matches!(ch, '`' | '"' | '\'' | '.' | ',' | ':' | ';'));
    if term.len() < 3 || term.len() > 96 || term.chars().any(char::is_whitespace) {
        return;
    }
    if term.chars().any(|ch| ch.is_ascii_alphanumeric()) {
        terms.insert(term.to_owned());
    }
}

fn is_code_like_query_token(token: &str) -> bool {
    token.contains('_')
        || token.contains('-')
        || token.chars().any(char::is_uppercase) && token.chars().any(char::is_lowercase)
}

fn is_source_intent_hint(term: &str) -> bool {
    matches!(
        term,
        "rust"
            | "source"
            | "source-file"
            | "repo-file"
            | "file"
            | "files"
            | "implementation"
            | "implementations"
            | "implements"
            | "defined"
            | "definition"
            | "function"
            | "functions"
            | "trait"
            | "traits"
            | "module"
            | "modules"
    )
}

fn is_natural_language_query_term(term: &str) -> bool {
    matches!(
        term,
        "which"
            | "where"
            | "what"
            | "who"
            | "when"
            | "why"
            | "how"
            | "only"
            | "output"
            | "answer"
            | "provided"
            | "automatic"
            | "system"
            | "most"
            | "likely"
            | "please"
            | "based"
            | "using"
            | "without"
            | "with"
            | "from"
            | "into"
            | "this"
            | "that"
            | "the"
            | "and"
            | "for"
            | "are"
            | "you"
            | "not"
    )
}

fn is_path_like_query_term(term: &str) -> bool {
    term.contains('/')
        || term.contains('\\')
        || term.ends_with(".rs")
        || term.ends_with(".toml")
        || term.ends_with(".md")
}

fn source_term_variants(token: &str) -> BTreeSet<String> {
    let mut variants = BTreeSet::new();
    let trimmed = token.trim_matches(|ch: char| matches!(ch, '`' | '"' | '\''));
    if trimmed.is_empty() {
        return variants;
    }
    let lower = trimmed.to_ascii_lowercase();
    variants.insert(lower.clone());
    variants.insert(lower.replace('-', "_"));
    variants.insert(lower.replace(['_', '-'], " "));
    variants.insert(lower.replace(['_', '-'], ""));
    let snake = camel_to_snake(trimmed);
    if !snake.is_empty() {
        variants.insert(snake.clone());
        variants.insert(snake.replace('_', ""));
    }
    variants
}

fn camel_to_snake(token: &str) -> String {
    let mut out = String::new();
    let mut previous_was_lower_or_digit = false;
    for ch in token.chars() {
        if ch.is_ascii_uppercase() {
            if previous_was_lower_or_digit && !out.ends_with('_') {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
            previous_was_lower_or_digit = false;
        } else if ch == '-' {
            if !out.ends_with('_') {
                out.push('_');
            }
            previous_was_lower_or_digit = false;
        } else {
            out.push(ch.to_ascii_lowercase());
            previous_was_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        }
    }
    out
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn explicit_query_paths(query: &str) -> impl Iterator<Item = PathBuf> + '_ {
    query
        .split(|ch: char| !is_query_path_char(ch))
        .flat_map(explicit_path_token_variants)
        .filter(|token| token.contains('/') || token.contains('.'))
        .filter(|token| !token.starts_with("http://") && !token.starts_with("https://"))
        .map(PathBuf::from)
}

fn is_query_path_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-')
}

fn explicit_path_token_variants(token: &str) -> Vec<&str> {
    let token = token.trim_matches(|ch: char| matches!(ch, '"' | '\'' | '`'));
    if token.is_empty() {
        return Vec::new();
    }
    let trimmed_sentence_dot = token.trim_end_matches('.');
    if trimmed_sentence_dot != token && !trimmed_sentence_dot.is_empty() {
        vec![token, trimmed_sentence_dot]
    } else {
        vec![token]
    }
}

fn lexical_query_terms(query: &str) -> BTreeSet<String> {
    query
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .map(str::trim)
        .filter(|term| term.len() >= 3)
        .map(str::to_lowercase)
        .filter(|term| {
            matches!(
                source_query_term_role(term),
                SourceQueryTermRole::SymbolLike
                    | SourceQueryTermRole::PathLike
                    | SourceQueryTermRole::LexicalHint
            )
        })
        .collect()
}

fn normalize_workspace_relative_path(workspace_root: &Path, path: &Path) -> Option<PathBuf> {
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return None;
    }
    let full_path = workspace_root.join(path);
    if !full_path.is_file() {
        return None;
    }
    Some(path.to_path_buf())
}

fn should_skip_repo_context_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
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
        })
}

fn should_skip_repo_context_path(relative: &Path) -> bool {
    relative.components().any(|component| {
        component.as_os_str().to_str().is_some_and(|name| {
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
        })
    })
}

fn is_secret_like_path(relative: &Path) -> bool {
    let text = relative.to_string_lossy().to_lowercase();
    text.ends_with(".env")
        || text.contains("/.env")
        || text.contains("id_rsa")
        || text.contains("id_ed25519")
        || text.contains("private_key")
        || text.ends_with(".pem")
        || text.ends_with(".key")
}

fn looks_like_text_file(relative: &Path) -> bool {
    relative
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_lowercase)
        .is_none_or(|extension| {
            matches!(
                extension.as_str(),
                "rs" | "toml"
                    | "md"
                    | "txt"
                    | "json"
                    | "yaml"
                    | "yml"
                    | "js"
                    | "ts"
                    | "tsx"
                    | "jsx"
                    | "py"
                    | "go"
                    | "java"
                    | "kt"
                    | "swift"
                    | "c"
                    | "h"
                    | "cpp"
                    | "hpp"
                    | "css"
                    | "html"
                    | "sh"
                    | "sql"
            )
        })
}

fn truncate_to_char_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    &text[..end]
}

fn snippet_window_around_byte(text: &str, byte_index: usize, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let before = max_bytes / 4;
    let mut start = byte_index.saturating_sub(before);
    while !text.is_char_boundary(start) {
        start = start.saturating_sub(1);
    }
    let mut end = start.saturating_add(max_bytes).min(text.len());
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    &text[start..end]
}

#[cfg(test)]
#[path = "tests/context_tests.rs"]
mod tests;
