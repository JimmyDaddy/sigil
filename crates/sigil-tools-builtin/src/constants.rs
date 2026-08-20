pub(crate) const DEFAULT_TEXT_LIMIT_BYTES: usize = 64 * 1024;
pub(crate) const HARD_TEXT_LIMIT_BYTES: usize = 256 * 1024;
pub(crate) const DEFAULT_READ_LIMIT_LINES: usize = 1000;
pub(crate) const HARD_READ_LIMIT_LINES: usize = 2000;
pub(crate) const MAX_MODEL_LINE_CHARS: usize = 2000;
pub(crate) const DEFAULT_LIST_LIMIT: usize = 200;
pub(crate) const DEFAULT_RECURSIVE_LIST_LIMIT: usize = 500;
pub(crate) const HARD_LIST_LIMIT: usize = 2000;
pub(crate) const DEFAULT_RECURSIVE_MAX_DEPTH: usize = 3;
pub(crate) const DEFAULT_GLOB_LIMIT: usize = 100;
pub(crate) const HARD_GLOB_LIMIT: usize = 1000;
pub(crate) const DEFAULT_GREP_LIMIT: usize = 100;
pub(crate) const HARD_GREP_LIMIT: usize = 1000;
pub(crate) const CHANGESET_ARTIFACT_ROOT: &str = "state/artifacts/changesets";
pub(crate) const WORKSPACE_TEMP_ROOT: &str = "cache/tmp";
pub(crate) const CHANGESET_PREVIEW_DIFF_FILE: &str = "preview.diff";
pub(crate) const CHANGESET_REVERSE_DIFF_FILE: &str = "reverse.diff";
pub(crate) const DEFAULT_CHANGESET_SUMMARY_LIMIT_BYTES: usize = 16 * 1024;
pub(crate) const DEFAULT_TERMINAL_READ_LIMIT_BYTES: usize = 16 * 1024;
pub(crate) const HARD_TERMINAL_READ_LIMIT_BYTES: usize = 128 * 1024;
pub(crate) const SIGIL_SCRATCH_DIR_ENV: &str = "SIGIL_SCRATCH_DIR";
/// RFC-0062 14.1: session-scoped scratch lives in `scratch_root/sessions/<session key>`.
pub(crate) const SESSION_SCRATCH_NAMESPACE_DIR: &str = "sessions";
/// Fallback namespace key for tool invocations without a durable session scope
/// (diagnostics and tests). Stable so repeated calls share one bounded namespace.
pub(crate) const NO_SESSION_SCRATCH_KEY: &str = "no-session";
/// Per-session scratch capacity. Checked deterministically before every scratch-using spawn.
pub(crate) const SCRATCH_QUOTA_PER_SESSION_BYTES: u64 = 512 * 1024 * 1024;
/// Aggregate hard cap across all session namespaces under one workspace scratch root.
pub(crate) const SCRATCH_QUOTA_WORKSPACE_HARD_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Namespaces whose last activity is older than this are eligible for TTL GC.
pub(crate) const SCRATCH_NAMESPACE_TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// Entry bound for the deterministic scratch usage/activity walk.
///
/// Directory depth is intentionally not bounded: build systems and test fixtures routinely
/// create deeply nested trees. The entry bound limits traversal work without making an otherwise
/// valid namespace unusable.
pub(crate) const SCRATCH_WALK_MAX_ENTRIES: usize = 100_000;
