use super::*;

pub(super) fn render_code_intelligence_trust_summary() -> Vec<String> {
    vec![
        render_config_readonly_row("Tool access", "read-only"),
        render_config_readonly_row("Server process", "local workspace LSP"),
        render_config_readonly_row("Write actions", "unavailable"),
    ]
}

pub(super) fn code_intelligence_overall_label(checks: &[DoctorCheck]) -> &'static str {
    if checks
        .iter()
        .any(|check| check.status == DoctorStatus::Error)
    {
        return DoctorStatus::Error.as_str();
    }
    if checks
        .iter()
        .any(|check| check.status == DoctorStatus::Warn)
    {
        return DoctorStatus::Warn.as_str();
    }
    DoctorStatus::Ok.as_str()
}

pub(super) fn render_code_intelligence_check_row(check: &DoctorCheck) -> String {
    format!(
        "- {}: {} · {}",
        check.name,
        check.status.as_str(),
        check.message
    )
}
