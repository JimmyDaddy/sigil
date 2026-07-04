use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use sigil_kernel::{SessionConfig, StorageConfig, StorageRoot};

pub const SIGIL_STATE_HOME_ENV: &str = "SIGIL_STATE_HOME";
pub const SIGIL_CACHE_HOME_ENV: &str = "SIGIL_CACHE_HOME";
pub const XDG_STATE_HOME_ENV: &str = "XDG_STATE_HOME";
pub const XDG_CACHE_HOME_ENV: &str = "XDG_CACHE_HOME";
pub const LOCALAPPDATA_ENV: &str = "LOCALAPPDATA";
pub const INPUT_HISTORY_FILE: &str = "input-history.jsonl";
pub const DEFAULT_SESSIONS_DIR: &str = "sessions";
pub const DEFAULT_ARTIFACTS_DIR: &str = "artifacts";
pub const DEFAULT_CHANGESETS_DIR: &str = "changesets";
pub const DEFAULT_TERMINAL_TASKS_DIR: &str = "tasks";
pub const DEFAULT_SCRATCH_DIR: &str = "tmp";
pub const DEFAULT_PROJECT_ASSETS_ROOT: &str = ".sigil";
pub const DEFAULT_WORKSPACE_SKILLS_DIR: &str = ".sigil/skills";
pub const DEFAULT_WORKSPACE_AGENTS_DIR: &str = ".sigil/agents";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigilPaths {
    pub workspace_root: PathBuf,
    pub workspace_id: String,
    pub state_root: PathBuf,
    pub cache_root: PathBuf,
    pub workspace_state_root: PathBuf,
    pub workspace_cache_root: PathBuf,
    pub session_log_dir: PathBuf,
    pub input_history_file: PathBuf,
    pub artifacts_root: PathBuf,
    pub changesets_root: PathBuf,
    pub terminal_tasks_root: PathBuf,
    pub scratch_root: PathBuf,
    pub project_assets_root: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoragePlatform {
    Macos,
    Linux,
    Windows,
}

#[derive(Debug, Clone)]
pub struct PathResolverEnv {
    pub platform: StoragePlatform,
    pub home_dir: Option<PathBuf>,
    pub sigil_state_home: Option<PathBuf>,
    pub sigil_cache_home: Option<PathBuf>,
    pub xdg_state_home: Option<PathBuf>,
    pub xdg_cache_home: Option<PathBuf>,
    pub local_app_data: Option<PathBuf>,
}

impl PathResolverEnv {
    #[must_use]
    pub fn current() -> Self {
        Self {
            platform: current_platform(),
            home_dir: home_dir(),
            sigil_state_home: env_path(SIGIL_STATE_HOME_ENV),
            sigil_cache_home: env_path(SIGIL_CACHE_HOME_ENV),
            xdg_state_home: env_path(XDG_STATE_HOME_ENV),
            xdg_cache_home: env_path(XDG_CACHE_HOME_ENV),
            local_app_data: env_path(LOCALAPPDATA_ENV),
        }
    }
}

/// Resolves all Sigil user-local state and cache paths for one workspace.
#[must_use]
pub fn resolve_sigil_paths(
    storage: &StorageConfig,
    session: &SessionConfig,
    workspace_root: impl AsRef<Path>,
) -> SigilPaths {
    resolve_sigil_paths_with_env(
        storage,
        session,
        workspace_root,
        &PathResolverEnv::current(),
    )
}

/// Resolves all Sigil paths with an explicit environment seam for tests.
#[must_use]
pub fn resolve_sigil_paths_with_env(
    storage: &StorageConfig,
    session: &SessionConfig,
    workspace_root: impl AsRef<Path>,
    env: &PathResolverEnv,
) -> SigilPaths {
    let raw_workspace_root = workspace_root.as_ref().to_path_buf();
    let project_assets_root =
        resolve_configured_path(&storage.project_assets_root, &raw_workspace_root);
    let workspace_root = canonical_or_absolute(workspace_root.as_ref());
    let workspace_id = workspace_id_for_root(&workspace_root);
    let state_root = resolve_state_root(storage, env);
    let cache_root = resolve_cache_root(storage, env);
    let workspace_state_root = state_root.join("workspaces").join(&workspace_id);
    let workspace_cache_root = cache_root.join("workspaces").join(&workspace_id);
    let session_log_dir = resolve_session_log_dir(session, &workspace_state_root);
    let input_history_file = workspace_state_root.join(INPUT_HISTORY_FILE);
    let artifacts_root = workspace_state_root.join(DEFAULT_ARTIFACTS_DIR);
    let changesets_root = artifacts_root.join(DEFAULT_CHANGESETS_DIR);
    let terminal_tasks_root = artifacts_root.join(DEFAULT_TERMINAL_TASKS_DIR);
    let scratch_root = workspace_cache_root.join(DEFAULT_SCRATCH_DIR);

    SigilPaths {
        workspace_root,
        workspace_id,
        state_root,
        cache_root,
        workspace_state_root,
        workspace_cache_root,
        session_log_dir,
        input_history_file,
        artifacts_root,
        changesets_root,
        terminal_tasks_root,
        scratch_root,
        project_assets_root,
    }
}

#[must_use]
pub fn workspace_id_for_root(workspace_root: &Path) -> String {
    let canonical = canonical_or_absolute(workspace_root);
    let slug = workspace_slug(&canonical);
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let hash = hex_lower(&hasher.finalize());
    format!("{slug}-{}", &hash[..12])
}

#[must_use]
pub fn project_asset_dir(
    workspace_root: &Path,
    project_assets_root: &Path,
    configured: &str,
    default_configured: &str,
    default_leaf: &str,
) -> PathBuf {
    if configured.trim() == default_configured {
        return project_assets_root.join(default_leaf);
    }
    resolve_configured_path(configured, workspace_root)
}

fn resolve_state_root(storage: &StorageConfig, env: &PathResolverEnv) -> PathBuf {
    if let Some(path) = &env.sigil_state_home {
        return path.clone();
    }
    match &storage.state_root {
        StorageRoot::Path(path) => PathBuf::from(path),
        StorageRoot::Auto => default_state_root(env),
    }
}

fn resolve_cache_root(storage: &StorageConfig, env: &PathResolverEnv) -> PathBuf {
    if let Some(path) = &env.sigil_cache_home {
        return path.clone();
    }
    match &storage.cache_root {
        StorageRoot::Path(path) => PathBuf::from(path),
        StorageRoot::Auto => default_cache_root(env),
    }
}

fn resolve_session_log_dir(session: &SessionConfig, workspace_state_root: &Path) -> PathBuf {
    match session.log_dir.as_deref() {
        Some(log_dir) => resolve_configured_path(log_dir, workspace_state_root),
        None => workspace_state_root.join(DEFAULT_SESSIONS_DIR),
    }
}

fn default_state_root(env: &PathResolverEnv) -> PathBuf {
    match env.platform {
        StoragePlatform::Macos => env
            .home_dir
            .as_ref()
            .map(|home| {
                home.join("Library")
                    .join("Application Support")
                    .join("sigil")
                    .join("state")
            })
            .unwrap_or_else(|| PathBuf::from(".sigil-state")),
        StoragePlatform::Linux => env
            .xdg_state_home
            .as_ref()
            .map(|root| root.join("sigil"))
            .or_else(|| {
                env.home_dir
                    .as_ref()
                    .map(|home| home.join(".local").join("state").join("sigil"))
            })
            .unwrap_or_else(|| PathBuf::from(".sigil-state")),
        StoragePlatform::Windows => env
            .local_app_data
            .as_ref()
            .map(|root| root.join("sigil").join("state"))
            .or_else(|| {
                env.home_dir.as_ref().map(|home| {
                    home.join("AppData")
                        .join("Local")
                        .join("sigil")
                        .join("state")
                })
            })
            .unwrap_or_else(|| PathBuf::from(".sigil-state")),
    }
}

fn default_cache_root(env: &PathResolverEnv) -> PathBuf {
    match env.platform {
        StoragePlatform::Macos => env
            .home_dir
            .as_ref()
            .map(|home| home.join("Library").join("Caches").join("sigil"))
            .unwrap_or_else(|| PathBuf::from(".sigil-cache")),
        StoragePlatform::Linux => env
            .xdg_cache_home
            .as_ref()
            .map(|root| root.join("sigil"))
            .or_else(|| {
                env.home_dir
                    .as_ref()
                    .map(|home| home.join(".cache").join("sigil"))
            })
            .unwrap_or_else(|| PathBuf::from(".sigil-cache")),
        StoragePlatform::Windows => env
            .local_app_data
            .as_ref()
            .map(|root| root.join("sigil").join("cache"))
            .or_else(|| {
                env.home_dir.as_ref().map(|home| {
                    home.join("AppData")
                        .join("Local")
                        .join("sigil")
                        .join("cache")
                })
            })
            .unwrap_or_else(|| PathBuf::from(".sigil-cache")),
    }
}

fn resolve_configured_path(configured: &str, base: &Path) -> PathBuf {
    let path = Path::new(configured);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn workspace_slug(workspace_root: &Path) -> String {
    let raw = workspace_root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("workspace");
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in raw.chars().flat_map(char::to_lowercase) {
        let allowed = ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.';
        if allowed {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "workspace".to_owned()
    } else {
        slug.to_owned()
    }
}

fn canonical_or_absolute(path: &Path) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(path) {
        return canonical;
    }
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name).and_then(non_empty_path)
}

fn non_empty_path(value: OsString) -> Option<PathBuf> {
    (!value.is_empty()).then(|| PathBuf::from(value))
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .and_then(non_empty_path)
        .or_else(|| env::var_os("USERPROFILE").and_then(non_empty_path))
}

fn current_platform() -> StoragePlatform {
    if cfg!(target_os = "macos") {
        StoragePlatform::Macos
    } else if cfg!(windows) {
        StoragePlatform::Windows
    } else {
        StoragePlatform::Linux
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn env(platform: StoragePlatform) -> PathResolverEnv {
        PathResolverEnv {
            platform,
            home_dir: Some(PathBuf::from("/home/alice")),
            sigil_state_home: None,
            sigil_cache_home: None,
            xdg_state_home: None,
            xdg_cache_home: None,
            local_app_data: Some(PathBuf::from("C:/Users/alice/AppData/Local")),
        }
    }

    #[test]
    fn resolves_linux_defaults_under_user_state_and_cache() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let paths = resolve_sigil_paths_with_env(
            &StorageConfig::default(),
            &SessionConfig::default(),
            workspace.path(),
            &env(StoragePlatform::Linux),
        );

        assert_eq!(
            paths.state_root,
            Path::new("/home/alice/.local/state/sigil")
        );
        assert_eq!(paths.cache_root, Path::new("/home/alice/.cache/sigil"));
        assert!(paths.workspace_id.contains('-'));
        assert!(paths.session_log_dir.ends_with("sessions"));
        assert!(paths.input_history_file.ends_with(INPUT_HISTORY_FILE));
        assert_eq!(paths.project_assets_root, workspace.path().join(".sigil"));
    }

    #[test]
    fn resolves_macos_defaults_under_application_support_and_caches() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let paths = resolve_sigil_paths_with_env(
            &StorageConfig::default(),
            &SessionConfig::default(),
            workspace.path(),
            &env(StoragePlatform::Macos),
        );

        assert_eq!(
            paths.state_root,
            Path::new("/home/alice/Library/Application Support/sigil/state")
        );
        assert_eq!(
            paths.cache_root,
            Path::new("/home/alice/Library/Caches/sigil")
        );
        assert_eq!(
            paths.session_log_dir,
            paths.workspace_state_root.join(DEFAULT_SESSIONS_DIR)
        );
    }

    #[test]
    fn resolves_windows_defaults_under_local_app_data() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let paths = resolve_sigil_paths_with_env(
            &StorageConfig::default(),
            &SessionConfig::default(),
            workspace.path(),
            &env(StoragePlatform::Windows),
        );

        assert_eq!(
            paths.state_root,
            Path::new("C:/Users/alice/AppData/Local/sigil/state")
        );
        assert_eq!(
            paths.cache_root,
            Path::new("C:/Users/alice/AppData/Local/sigil/cache")
        );
    }

    #[test]
    fn resolves_windows_defaults_from_home_when_local_app_data_is_missing() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let mut resolver_env = env(StoragePlatform::Windows);
        resolver_env.local_app_data = None;

        let paths = resolve_sigil_paths_with_env(
            &StorageConfig::default(),
            &SessionConfig::default(),
            workspace.path(),
            &resolver_env,
        );

        assert_eq!(
            paths.state_root,
            Path::new("/home/alice/AppData/Local/sigil/state")
        );
        assert_eq!(
            paths.cache_root,
            Path::new("/home/alice/AppData/Local/sigil/cache")
        );
    }

    #[test]
    fn resolver_defaults_to_relative_roots_without_home_or_xdg() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let mut resolver_env = env(StoragePlatform::Linux);
        resolver_env.home_dir = None;
        resolver_env.xdg_state_home = None;
        resolver_env.xdg_cache_home = None;

        let paths = resolve_sigil_paths_with_env(
            &StorageConfig::default(),
            &SessionConfig::default(),
            workspace.path().join("missing-workspace"),
            &resolver_env,
        );

        assert_eq!(paths.state_root, Path::new(".sigil-state"));
        assert_eq!(paths.cache_root, Path::new(".sigil-cache"));
        assert!(paths.workspace_id.starts_with("missing-workspace-"));
    }

    #[test]
    fn workspace_slug_and_absolute_fallback_cover_empty_and_relative_paths() {
        assert_eq!(workspace_slug(Path::new("/")), "workspace");

        let relative = Path::new("definitely-missing-sigil-workspace");
        let resolved = canonical_or_absolute(relative);
        assert!(resolved.ends_with(relative));
        assert!(resolved.is_absolute());
    }

    #[test]
    fn workspace_id_is_stable_sanitized_and_hash_isolated() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("one").join("Project Name!");
        let second = temp.path().join("two").join("Project Name!");
        std::fs::create_dir_all(&first).expect("first workspace should create");
        std::fs::create_dir_all(&second).expect("second workspace should create");

        let first_id = workspace_id_for_root(&first);
        let first_again = workspace_id_for_root(&first.join("."));
        let second_id = workspace_id_for_root(&second);

        assert_eq!(first_id, first_again);
        assert_ne!(first_id, second_id);
        assert!(first_id.starts_with("project-name-"));
        assert!(second_id.starts_with("project-name-"));
    }

    #[test]
    fn env_overrides_win_over_configured_roots() {
        let storage = StorageConfig {
            state_root: StorageRoot::Path("/configured/state".to_owned()),
            cache_root: StorageRoot::Path("/configured/cache".to_owned()),
            ..StorageConfig::default()
        };
        let mut resolver_env = env(StoragePlatform::Windows);
        resolver_env.sigil_state_home = Some(PathBuf::from("/override/state"));
        resolver_env.sigil_cache_home = Some(PathBuf::from("/override/cache"));

        let paths = resolve_sigil_paths_with_env(
            &storage,
            &SessionConfig::default(),
            "/workspace/project",
            &resolver_env,
        );

        assert_eq!(paths.state_root, Path::new("/override/state"));
        assert_eq!(paths.cache_root, Path::new("/override/cache"));
    }

    #[test]
    fn relative_session_override_resolves_under_workspace_state_root() {
        let session = SessionConfig {
            log_dir: Some("custom-sessions".to_owned()),
        };
        let paths = resolve_sigil_paths_with_env(
            &StorageConfig::default(),
            &session,
            "/workspace/project",
            &env(StoragePlatform::Linux),
        );

        assert_eq!(
            paths.session_log_dir,
            paths.workspace_state_root.join("custom-sessions")
        );
    }
}
