use super::*;

pub(super) const WORKSPACE_INSTRUCTION_FILES: &[&str] =
    &["SIGIL.md", "AGENTS.md", "CLAUDE.md", "SIGIL.local.md"];

pub(super) fn workspace_instruction_files(workspace_root: &Path) -> Vec<PathBuf> {
    WORKSPACE_INSTRUCTION_FILES
        .iter()
        .map(|file| workspace_root.join(file))
        .filter(|path| path.is_file())
        .map(|path| {
            path.strip_prefix(workspace_root)
                .map(Path::to_path_buf)
                .unwrap_or(path)
        })
        .collect()
}

pub(super) fn repo_instruction_trust_summary(count: usize, trust: WorkspaceTrust) -> String {
    let label = repo_instruction_trust_label(trust);
    if count == 1 {
        format!("1 file · {label}")
    } else {
        format!("{count} files · {label}")
    }
}

pub(super) fn repo_verification_candidate_summary(count: usize, trust: WorkspaceTrust) -> String {
    if count == 0 {
        return "none found".to_owned();
    }
    let policy = if trust == WorkspaceTrust::Trusted {
        "available to task checks"
    } else {
        "review required"
    };
    format!("{count} found · {policy}")
}

pub(super) fn repo_instruction_trust_label(trust: WorkspaceTrust) -> &'static str {
    match trust {
        WorkspaceTrust::Trusted => "trusted instructions",
        WorkspaceTrust::Unknown | WorkspaceTrust::Restricted | WorkspaceTrust::Denied => {
            "untrusted data"
        }
    }
}

pub(super) fn workspace_trust_label(trust: WorkspaceTrust) -> &'static str {
    match trust {
        WorkspaceTrust::Unknown => "unknown",
        WorkspaceTrust::Trusted => "trusted",
        WorkspaceTrust::Restricted => "restricted",
        WorkspaceTrust::Denied => "denied",
    }
}

#[cfg(test)]
pub(crate) fn repo_check_promotion_requirement(effect: sigil_kernel::ToolEffect) -> &'static str {
    if effect.may_mutate_workspace() {
        "workspace-trust/approval+rerun-readonly-check"
    } else {
        "workspace-trust/approval"
    }
}
