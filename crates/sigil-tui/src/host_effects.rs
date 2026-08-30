//! Capabilities owned by the terminal host.
//!
//! Application and input code emits typed host requests; only the launcher injects a concrete
//! implementation. This keeps clipboard, image capture and external opening out of the
//! normalized input contract and makes tests deterministic without spawning host processes.

use std::{ffi::OsString, path::Path};

#[cfg(not(test))]
use std::process::Command;

#[cfg(not(test))]
use anyhow::Context;
use anyhow::Result;

use crate::clipboard::ClipboardCopyOutcome;

#[derive(Debug, Clone, Copy)]
pub(crate) enum ExternalLaunchPlatform {
    MacOs,
    Windows,
    Freedesktop,
    Unsupported,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ExternalLaunchTarget<'a> {
    Url(&'a str),
    RevealFile(&'a Path),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalLaunchPlan {
    pub(crate) program: &'static str,
    pub(crate) args: Vec<OsString>,
}

pub(crate) trait HostEffects {
    #[allow(dead_code)]
    fn read_clipboard_image_png(&mut self) -> Result<Option<Vec<u8>>>;
    fn copy_text(&mut self, text: &str, osc52_enabled: bool) -> ClipboardCopyOutcome;
    fn launch_external(&mut self, target: ExternalLaunchTarget<'_>) -> Result<()>;
}

#[cfg(not(test))]
#[derive(Debug, Default)]
pub(crate) struct SystemHostEffects;

#[cfg(not(test))]
impl HostEffects for SystemHostEffects {
    fn read_clipboard_image_png(&mut self) -> Result<Option<Vec<u8>>> {
        crate::clipboard_image::read_clipboard_image_png()
    }

    fn copy_text(&mut self, text: &str, osc52_enabled: bool) -> ClipboardCopyOutcome {
        crate::clipboard::copy_text(text, osc52_enabled)
    }

    fn launch_external(&mut self, target: ExternalLaunchTarget<'_>) -> Result<()> {
        let plan = external_launch_plan(target, current_external_launch_platform())?;
        Command::new(plan.program)
            .args(plan.args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| format!("failed to launch {}", plan.program))?;
        Ok(())
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct TestHostEffects;

#[cfg(test)]
impl HostEffects for TestHostEffects {
    fn read_clipboard_image_png(&mut self) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    fn copy_text(&mut self, text: &str, osc52_enabled: bool) -> ClipboardCopyOutcome {
        // Preserve the existing clipboard contract for unit tests while keeping the capability
        // injected at the launcher boundary. External process opening remains plan-only.
        crate::clipboard::copy_text(text, osc52_enabled)
    }

    fn launch_external(&mut self, target: ExternalLaunchTarget<'_>) -> Result<()> {
        external_launch_plan(target, current_external_launch_platform()).map(|_| ())
    }
}

pub(crate) fn external_launch_plan(
    target: ExternalLaunchTarget<'_>,
    platform: ExternalLaunchPlatform,
) -> Result<ExternalLaunchPlan> {
    if let ExternalLaunchTarget::Url(url) = target
        && (!url.starts_with("https://") || url.chars().any(char::is_control))
    {
        anyhow::bail!("external URL must be a valid HTTPS URL");
    }

    let plan = match (platform, target) {
        (ExternalLaunchPlatform::MacOs, ExternalLaunchTarget::Url(url)) => ExternalLaunchPlan {
            program: "/usr/bin/open",
            args: vec![OsString::from(url)],
        },
        (ExternalLaunchPlatform::MacOs, ExternalLaunchTarget::RevealFile(path)) => {
            ExternalLaunchPlan {
                program: "/usr/bin/open",
                args: vec![OsString::from("-R"), path.as_os_str().to_owned()],
            }
        }
        (ExternalLaunchPlatform::Windows, ExternalLaunchTarget::Url(url)) => ExternalLaunchPlan {
            program: "rundll32.exe",
            args: vec![
                OsString::from("url.dll,FileProtocolHandler"),
                OsString::from(url),
            ],
        },
        (ExternalLaunchPlatform::Windows, ExternalLaunchTarget::RevealFile(path)) => {
            let mut select_arg = OsString::from("/select,");
            select_arg.push(path.as_os_str());
            ExternalLaunchPlan {
                program: "explorer.exe",
                args: vec![select_arg],
            }
        }
        (ExternalLaunchPlatform::Freedesktop, ExternalLaunchTarget::Url(url)) => {
            ExternalLaunchPlan {
                program: "xdg-open",
                args: vec![OsString::from(url)],
            }
        }
        (ExternalLaunchPlatform::Freedesktop, ExternalLaunchTarget::RevealFile(path)) => {
            let parent = path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("support report has no parent directory"))?;
            ExternalLaunchPlan {
                program: "xdg-open",
                args: vec![parent.as_os_str().to_owned()],
            }
        }
        (ExternalLaunchPlatform::Unsupported, _) => {
            anyhow::bail!("external opening is not supported on this platform");
        }
    };
    Ok(plan)
}

const fn current_external_launch_platform() -> ExternalLaunchPlatform {
    if cfg!(target_os = "macos") {
        ExternalLaunchPlatform::MacOs
    } else if cfg!(target_os = "windows") {
        ExternalLaunchPlatform::Windows
    } else if cfg!(unix) {
        ExternalLaunchPlatform::Freedesktop
    } else {
        ExternalLaunchPlatform::Unsupported
    }
}
