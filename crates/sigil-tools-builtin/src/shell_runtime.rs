use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

#[cfg(windows)]
use crate::execution_backends::find_executable_on_path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellDialect {
    Posix,
    PowerShell,
    Cmd,
}

impl ShellDialect {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Posix => "posix",
            Self::PowerShell => "powershell",
            Self::Cmd => "cmd",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedShell {
    program: PathBuf,
    dialect: ShellDialect,
}

impl ResolvedShell {
    pub(crate) fn detect_default() -> Self {
        #[cfg(windows)]
        {
            let program = find_executable_on_path("pwsh.exe")
                .unwrap_or_else(|| PathBuf::from("powershell.exe"));
            return Self {
                program,
                dialect: ShellDialect::PowerShell,
            };
        }

        #[cfg(not(windows))]
        {
            Self {
                program: PathBuf::from("sh"),
                dialect: ShellDialect::Posix,
            }
        }
    }

    pub(crate) fn resolve_explicit(program: impl AsRef<Path>) -> Result<Self> {
        let program = program.as_ref();
        let Some(file_name) = program.file_name().and_then(|name| name.to_str()) else {
            bail!("terminal shell must name a supported executable");
        };
        let stem = file_name
            .strip_suffix(".exe")
            .unwrap_or(file_name)
            .to_ascii_lowercase();
        let dialect = match stem.as_str() {
            "sh" | "bash" | "zsh" | "fish" => ShellDialect::Posix,
            "pwsh" | "powershell" => ShellDialect::PowerShell,
            "cmd" => ShellDialect::Cmd,
            _ => bail!(
                "unsupported terminal shell `{}`; supported shells are sh/bash/zsh/fish, pwsh/powershell, and cmd",
                program.display()
            ),
        };
        Ok(Self {
            program: program.to_path_buf(),
            dialect,
        })
    }

    pub(crate) fn program(&self) -> &Path {
        &self.program
    }

    pub(crate) fn program_string(&self) -> String {
        self.program.to_string_lossy().into_owned()
    }

    pub(crate) const fn dialect(&self) -> ShellDialect {
        self.dialect
    }

    pub(crate) fn one_shot_args(&self, command: &str) -> Vec<String> {
        match self.dialect {
            ShellDialect::Posix => vec!["-c".to_owned(), command.to_owned()],
            ShellDialect::PowerShell => powershell_args(command),
            ShellDialect::Cmd => cmd_args(command),
        }
    }

    pub(crate) fn terminal_args(&self, command: &str) -> Vec<String> {
        match self.dialect {
            ShellDialect::Posix => vec!["-lc".to_owned(), command.to_owned()],
            ShellDialect::PowerShell => powershell_args(command),
            ShellDialect::Cmd => cmd_args(command),
        }
    }
}

fn powershell_args(command: &str) -> Vec<String> {
    let script = format!(
        "$OutputEncoding = [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false);\n\
         $global:LASTEXITCODE = $null;\n\
         {command}\n\
         $__sigil_success = $?; $__sigil_exit = $global:LASTEXITCODE;\n\
         if ($null -ne $__sigil_exit) {{ exit $__sigil_exit }};\n\
         if (-not $__sigil_success) {{ exit 1 }}"
    );
    vec![
        "-NoLogo".to_owned(),
        "-NoProfile".to_owned(),
        "-NonInteractive".to_owned(),
        "-Command".to_owned(),
        script,
    ]
}

fn cmd_args(command: &str) -> Vec<String> {
    vec![
        "/d".to_owned(),
        "/s".to_owned(),
        "/c".to_owned(),
        format!("chcp 65001>nul & {command}"),
    ]
}
