use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result};
use sigil_code_intel::context::{CodeContextBuilder, CodeContextHit};
use sigil_kernel::{
    ContextItem, ContextSensitivity, PluginHookContextItems, PluginHookContextOptions,
    PluginHookOutputEnvelope, RuntimeContextCandidates, TaskMemoryV1,
    plugin_hook_output_context_items, task_memory_context_items,
};

const REPO_CONTEXT_MAX_FILES_SCANNED: usize = 160;
const REPO_CONTEXT_MAX_ITEMS: usize = 3;
const REPO_CONTEXT_MAX_BYTES_PER_FILE: usize = 8 * 1024;
const REPO_CONTEXT_SNIPPET_MAX_BYTES: usize = 2 * 1024;

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

    let mut candidates = BTreeMap::<PathBuf, f32>::new();
    for path in explicit_query_paths(query) {
        if let Some(relative) = normalize_workspace_relative_path(&workspace_root, &path) {
            candidates.entry(relative).or_insert(100.0);
        }
    }

    let explicit_candidate_count = candidates.len();
    let terms = lexical_query_terms(query);
    if explicit_candidate_count == 0 && !terms.is_empty() {
        collect_lexical_file_candidates(&workspace_root, &terms, &mut candidates);
    }

    let builder = CodeContextBuilder::new();
    let mut runtime = RuntimeContextCandidates::default();
    let mut ranked = candidates.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    for (relative_path, score) in ranked.into_iter().take(REPO_CONTEXT_MAX_ITEMS) {
        let hit = repo_context_hit(&workspace_root, &builder, relative_path, score);
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
    candidates: &mut BTreeMap<PathBuf, f32>,
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
                candidates
                    .entry(relative.to_path_buf())
                    .and_modify(|existing| *existing = existing.max(score))
                    .or_insert(score);
            }
        }
        if scanned >= REPO_CONTEXT_MAX_FILES_SCANNED {
            break;
        }
    }
}

fn repo_context_hit(
    workspace_root: &Path,
    builder: &CodeContextBuilder,
    relative_path: PathBuf,
    score: f32,
) -> CodeContextHit {
    if is_secret_like_path(&relative_path) {
        let mut hit = builder
            .clone()
            .sensitivity(ContextSensitivity::Secret)
            .repo_file_hit(
                relative_path,
                "secret-like repository file omitted from automatic context",
            );
        hit.item.score = Some(score);
        return hit;
    }

    let full_path = workspace_root.join(&relative_path);
    let body = read_repo_context_snippet(&full_path).unwrap_or_else(|| {
        format!(
            "repository file {} could not be read for automatic context",
            relative_path.display()
        )
    });
    let mut hit = builder.repo_file_hit(relative_path, body);
    hit.item.score = Some(score);
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

fn explicit_query_paths(query: &str) -> impl Iterator<Item = PathBuf> + '_ {
    query
        .split_whitespace()
        .map(|token| {
            token.trim_matches(|ch: char| {
                matches!(
                    ch,
                    '"' | '\''
                        | '`'
                        | ','
                        | ';'
                        | ':'
                        | '，'
                        | '。'
                        | '：'
                        | '；'
                        | '（'
                        | '）'
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                )
            })
        })
        .filter(|token| token.contains('/') || token.contains('.'))
        .filter(|token| !token.starts_with("http://") && !token.starts_with("https://"))
        .map(PathBuf::from)
}

fn lexical_query_terms(query: &str) -> BTreeSet<String> {
    query
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .map(str::trim)
        .filter(|term| term.len() >= 3)
        .map(str::to_lowercase)
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

#[cfg(test)]
#[path = "tests/context_tests.rs"]
mod tests;
