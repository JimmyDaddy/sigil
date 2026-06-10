use std::{ffi::OsString, fs};

use super::*;

#[test]
fn discovery_finds_installed_rust_server_from_workspace_marker() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).expect("bin should build");
    fs::write(temp.path().join("Cargo.toml"), "[package]\nname='x'\n").expect("cargo should write");
    fs::write(bin.join("rust-analyzer"), "").expect("server binary should write");
    let path_env = OsString::from(bin.as_os_str());

    let discovered =
        discover_language_servers_with_path(temp.path(), true, Some(path_env.as_os_str()))
            .expect("discovery should succeed");

    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].config.name, "rust-analyzer");
    assert_eq!(discovered[0].availability, ServerAvailability::Installed);
    assert_eq!(discovered[0].source, DiscoverySource::BuiltIn);
}

#[test]
fn rust_analyzer_profile_uses_lightweight_tui_initialization() {
    let profile = built_in_profiles()
        .into_iter()
        .find(|profile| profile.name == "rust-analyzer")
        .expect("rust analyzer profile should exist");

    assert_eq!(profile.initialization_options["checkOnSave"], false);
    assert_eq!(
        profile.initialization_options["cachePriming"]["enable"],
        false
    );
    assert_eq!(
        profile.initialization_options["cargo"]["buildScripts"]["enable"],
        false
    );
    assert_eq!(profile.initialization_options["cargo"]["allTargets"], false);
    assert_eq!(profile.initialization_options["procMacro"]["enable"], false);
}

#[test]
fn discovery_reports_missing_typescript_server() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::write(temp.path().join("package.json"), "{}\n").expect("package should write");

    let discovered = discover_language_servers_with_path(temp.path(), true, Some("".as_ref()))
        .expect("discovery should succeed");

    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].config.name, "typescript-language-server");
    assert_eq!(discovered[0].availability, ServerAvailability::Missing);
    assert_eq!(
        discovered[0].install_hint.as_deref(),
        Some("install typescript-language-server")
    );
}

#[test]
fn discovery_can_suppress_missing_servers() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::write(temp.path().join("main.go"), "package main\n").expect("go file should write");

    let discovered = discover_language_servers_with_path(temp.path(), false, Some("".as_ref()))
        .expect("discovery should succeed");

    assert!(discovered.is_empty());
}

#[test]
fn discovery_skips_dependency_directories() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let dependency = temp.path().join("node_modules").join("pkg");
    fs::create_dir_all(&dependency).expect("dependency dir should build");
    fs::write(dependency.join("index.ts"), "export const x = 1;\n")
        .expect("dependency file should write");

    let discovered = discover_language_servers_with_path(temp.path(), true, Some("".as_ref()))
        .expect("discovery should succeed");

    assert!(discovered.is_empty());
}

#[test]
fn discovery_matches_multiple_languages() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).expect("bin should build");
    fs::write(temp.path().join("Cargo.toml"), "[package]\nname='x'\n").expect("cargo should write");
    fs::write(temp.path().join("pyproject.toml"), "[project]\nname='x'\n")
        .expect("python marker should write");
    fs::write(bin.join("rust-analyzer"), "").expect("rust server should write");
    fs::write(bin.join("pyright-langserver"), "").expect("python server should write");
    let path_env = OsString::from(bin.as_os_str());

    let discovered =
        discover_language_servers_with_path(temp.path(), true, Some(path_env.as_os_str()))
            .expect("discovery should succeed");
    let names = discovered
        .iter()
        .map(|server| server.config.name.as_str())
        .collect::<Vec<_>>();

    assert_eq!(names, vec!["rust-analyzer", "pyright-langserver"]);
    assert!(
        discovered
            .iter()
            .all(|server| server.availability == ServerAvailability::Installed)
    );
}
