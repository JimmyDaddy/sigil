use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use ignore::{DirEntry, WalkBuilder};
use serde_json::{Value, json};
use sigil_kernel::LanguageServerConfig;

const MAX_DISCOVERY_ENTRIES: usize = 20_000;
const SKIPPED_DIRS: &[&str] = &[
    ".git",
    ".sigil",
    "target",
    "node_modules",
    "dist",
    "build",
    ".venv",
    "__pycache__",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoverySource {
    BuiltIn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerAvailability {
    Installed,
    Missing,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredLanguageServer {
    pub config: LanguageServerConfig,
    pub source: DiscoverySource,
    pub availability: ServerAvailability,
    pub install_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LanguageServerProfile {
    pub name: &'static str,
    pub languages: &'static [&'static str],
    pub command: &'static str,
    pub args: &'static [&'static str],
    pub root_markers: &'static [&'static str],
    pub file_extensions: &'static [&'static str],
    pub initialization_options: Value,
    pub startup_timeout_ms: u64,
    pub install_hint: Option<&'static str>,
}

impl LanguageServerProfile {
    pub(crate) fn to_config(&self) -> LanguageServerConfig {
        LanguageServerConfig {
            name: self.name.to_owned(),
            languages: self
                .languages
                .iter()
                .map(|language| (*language).to_owned())
                .collect(),
            command: self.command.to_owned(),
            args: self.args.iter().map(|arg| (*arg).to_owned()).collect(),
            env: BTreeMap::new(),
            root_markers: self
                .root_markers
                .iter()
                .map(|marker| (*marker).to_owned())
                .collect(),
            file_extensions: self
                .file_extensions
                .iter()
                .map(|extension| (*extension).to_owned())
                .collect(),
            initialization_options: self.initialization_options.clone(),
            trust_required: true,
            startup_timeout_ms: self.startup_timeout_ms,
        }
    }

    fn matches(&self, workspace_signals: &WorkspaceSignals) -> bool {
        self.root_markers
            .iter()
            .any(|marker| workspace_signals.markers.contains(*marker))
            || self
                .file_extensions
                .iter()
                .any(|extension| workspace_signals.extensions.contains(*extension))
    }
}

#[derive(Debug, Default)]
struct WorkspaceSignals {
    markers: BTreeSet<String>,
    extensions: BTreeSet<String>,
}

pub(crate) fn built_in_profiles() -> Vec<LanguageServerProfile> {
    vec![
        LanguageServerProfile {
            name: "rust-analyzer",
            languages: &["rust"],
            command: "rust-analyzer",
            args: &[],
            root_markers: &["Cargo.toml", "rust-project.json"],
            file_extensions: &["rs"],
            initialization_options: json!({
                "cachePriming": { "enable": false },
                "cargo": {
                    "allTargets": false,
                    "buildScripts": { "enable": false }
                },
                "checkOnSave": false,
                "procMacro": { "enable": false }
            }),
            startup_timeout_ms: 10_000,
            install_hint: Some("install rust-analyzer"),
        },
        LanguageServerProfile {
            name: "typescript-language-server",
            languages: &["typescript", "javascript"],
            command: "typescript-language-server",
            args: &["--stdio"],
            root_markers: &["package.json", "tsconfig.json", "jsconfig.json"],
            file_extensions: &["ts", "tsx", "js", "jsx", "mjs", "cjs"],
            initialization_options: Value::Null,
            startup_timeout_ms: 10_000,
            install_hint: Some("install typescript-language-server"),
        },
        LanguageServerProfile {
            name: "pyright-langserver",
            languages: &["python"],
            command: "pyright-langserver",
            args: &["--stdio"],
            root_markers: &["pyproject.toml", "setup.py", "requirements.txt"],
            file_extensions: &["py"],
            initialization_options: Value::Null,
            startup_timeout_ms: 10_000,
            install_hint: Some("install pyright"),
        },
        LanguageServerProfile {
            name: "gopls",
            languages: &["go"],
            command: "gopls",
            args: &[],
            root_markers: &["go.mod"],
            file_extensions: &["go"],
            initialization_options: Value::Null,
            startup_timeout_ms: 10_000,
            install_hint: Some("install gopls"),
        },
    ]
}

pub fn discover_language_servers(
    workspace_root: &Path,
    report_missing: bool,
) -> Result<Vec<DiscoveredLanguageServer>> {
    discover_language_servers_with_path(
        workspace_root,
        report_missing,
        env::var_os("PATH").as_deref(),
    )
}

pub(crate) fn discover_language_servers_with_path(
    workspace_root: &Path,
    report_missing: bool,
    path_env: Option<&OsStr>,
) -> Result<Vec<DiscoveredLanguageServer>> {
    let signals = collect_workspace_signals(workspace_root)?;
    let mut discovered = Vec::new();
    for profile in built_in_profiles() {
        if !profile.matches(&signals) {
            continue;
        }
        let installed = command_exists(profile.command, path_env);
        if !installed && !report_missing {
            continue;
        }
        discovered.push(DiscoveredLanguageServer {
            config: profile.to_config(),
            source: DiscoverySource::BuiltIn,
            availability: if installed {
                ServerAvailability::Installed
            } else {
                ServerAvailability::Missing
            },
            install_hint: profile.install_hint.map(str::to_owned),
        });
    }
    Ok(discovered)
}

fn collect_workspace_signals(workspace_root: &Path) -> Result<WorkspaceSignals> {
    let mut signals = WorkspaceSignals::default();
    let walker = WalkBuilder::new(workspace_root)
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .parents(true)
        .filter_entry(should_enter)
        .build();
    for entry in walker.take(MAX_DISCOVERY_ENTRIES) {
        let entry = entry.with_context(|| {
            format!(
                "failed to scan workspace for language server discovery under {}",
                workspace_root.display()
            )
        })?;
        let path = entry.path();
        if entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            if let Some(name) = path.file_name().and_then(OsStr::to_str) {
                signals.markers.insert(name.to_owned());
            }
            if let Some(extension) = path
                .extension()
                .and_then(OsStr::to_str)
                .map(|extension| extension.trim_start_matches('.').to_ascii_lowercase())
                .filter(|extension| !extension.is_empty())
            {
                signals.extensions.insert(extension);
            }
        }
    }
    Ok(signals)
}

fn should_enter(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return true;
    }
    if !entry
        .file_type()
        .is_some_and(|file_type| file_type.is_dir())
    {
        return true;
    }
    entry
        .file_name()
        .to_str()
        .is_none_or(|name| !SKIPPED_DIRS.contains(&name))
}

fn command_exists(command: &str, path_env: Option<&OsStr>) -> bool {
    let command_path = Path::new(command);
    if command_path.components().count() != 1 {
        return false;
    }
    let Some(path_env) = path_env else {
        return false;
    };
    env::split_paths(path_env).any(|dir| {
        if dir.as_os_str().is_empty() {
            return false;
        }
        command_candidates(&dir, command)
            .into_iter()
            .any(|candidate| candidate.is_file())
    })
}

fn command_candidates(dir: &Path, command: &str) -> Vec<PathBuf> {
    #[cfg(not(windows))]
    {
        vec![dir.join(command)]
    }
    #[cfg(windows)]
    {
        let mut candidates = vec![dir.join(command)];
        if Path::new(command).extension().is_none() {
            candidates.push(dir.join(format!("{command}.exe")));
            candidates.push(dir.join(format!("{command}.cmd")));
            candidates.push(dir.join(format!("{command}.bat")));
        }
        candidates
    }
}

#[cfg(test)]
#[path = "tests/discovery_tests.rs"]
mod tests;
