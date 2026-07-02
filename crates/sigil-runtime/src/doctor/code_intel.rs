use super::*;

/// Builds only the code-intelligence diagnostics without starting language servers.
#[must_use]
pub fn build_code_intelligence_checks(
    root_config: &RootConfig,
    workspace_root: &Path,
) -> Vec<DoctorCheck> {
    let mut report = DoctorReport::default();
    check_code_intelligence(&mut report, root_config, workspace_root);
    report.checks
}

pub(super) fn check_code_intelligence(
    report: &mut DoctorReport,
    root_config: &RootConfig,
    workspace_root: &Path,
) {
    if !sigil_code_intel::workspace::config_enabled(&root_config.code_intelligence) {
        report.push(DoctorStatus::Ok, "code_intelligence", "disabled");
        return;
    }

    let plan = sigil_code_intel::workspace::effective_server_plan(
        &root_config.code_intelligence,
        workspace_root,
    );
    if plan.statuses.is_empty() {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "code_intelligence",
            "enabled but no language server plan was produced",
            Some("add code_intelligence.servers entries, or disable [code_intelligence].enabled"),
        );
        return;
    }

    for status in &plan.statuses {
        let server = plan
            .servers
            .iter()
            .find(|server| server.name == status.server);
        let command_status = server
            .map(|server| command_status(&server.command, workspace_root))
            .unwrap_or(CommandStatus::Missing);
        let status_level = match status.status.as_str() {
            "installed" | "configured" if command_status == CommandStatus::Available => {
                DoctorStatus::Ok
            }
            "installed" | "configured" => DoctorStatus::Warn,
            "missing" | "disabled" => DoctorStatus::Warn,
            value if value.starts_with("degraded") => DoctorStatus::Warn,
            _ => DoctorStatus::Warn,
        };
        let remediation = lsp_remediation(status.status.as_str(), command_status, &status.server);
        report.push_with_remediation(
            status_level,
            format!("lsp:{}", status.server),
            format!(
                "{} languages={} command={}",
                status.status,
                status.languages.join(","),
                command_status.as_str()
            ),
            remediation,
        );
    }
}

fn lsp_remediation(
    status: &str,
    command_status: CommandStatus,
    server_name: &str,
) -> Option<String> {
    if command_status == CommandStatus::Available && matches!(status, "installed" | "configured") {
        return None;
    }
    Some(format!(
        "install or configure the {server_name} language server command, or disable code intelligence for this workspace",
    ))
}
