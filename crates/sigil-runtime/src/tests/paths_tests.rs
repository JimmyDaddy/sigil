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
    assert_eq!(
        paths.session_exports_root,
        paths.workspace_state_root.join(DEFAULT_SESSION_EXPORTS_DIR)
    );
    assert!(paths.input_history_file.ends_with(INPUT_HISTORY_FILE));
    assert_eq!(
        paths.project_assets_root,
        paths.workspace_root.join(".sigil")
    );
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
