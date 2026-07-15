use sigil_runtime::doctor::{
    DoctorCheck, DoctorReport, DoctorReportOptions, DoctorStatus, build_doctor_report_with_options,
};

use super::{AppState, TimelineRole};
use crate::appearance_diagnostics::appearance_doctor_checks;

impl AppState {
    pub(super) fn build_tui_doctor_report(&self) -> DoctorReport {
        let plugin_projection = sigil_kernel::PluginStateProjection::from_entries(
            &self.session_browser.current_entries,
        );
        let plugin_trust_entries = plugin_projection
            .trust_entries
            .values()
            .cloned()
            .collect::<Vec<_>>();
        build_doctor_report_with_options(
            &self.config_path,
            &self.workspace_root,
            DoctorReportOptions {
                appearance_checks: Some(&appearance_doctor_checks),
                plugin_trust_entries: Some(&plugin_trust_entries),
            },
        )
    }

    pub(super) fn show_doctor_report(&mut self) {
        let report = self.build_tui_doctor_report();
        let status = report.overall_status().as_str();
        self.last_notice = Some(format!("doctor: {status}"));
        self.push_event("doctor", status);
        self.push_timeline(TimelineRole::Notice, render_doctor_report(&report));
    }
}

fn render_doctor_report(report: &DoctorReport) -> String {
    let counts = doctor_status_counts(report);
    let mut lines = vec![
        format!("doctor: {}", report.overall_status().as_str()),
        format!(
            "summary: {} error · {} warn · {} ok",
            counts.errors, counts.warnings, counts.ok
        ),
    ];
    push_doctor_attention_section(report, &mut lines);
    lines.push("checks:".to_owned());
    for check in &report.checks {
        push_doctor_check_lines(check, "", "  ", &mut lines);
    }
    lines.join("\n")
}

#[derive(Debug, Default, PartialEq, Eq)]
struct DoctorStatusCounts {
    ok: usize,
    warnings: usize,
    errors: usize,
}

fn doctor_status_counts(report: &DoctorReport) -> DoctorStatusCounts {
    let mut counts = DoctorStatusCounts::default();
    for check in &report.checks {
        match check.status {
            DoctorStatus::Ok => counts.ok += 1,
            DoctorStatus::Warn => counts.warnings += 1,
            DoctorStatus::Error => counts.errors += 1,
        }
    }
    counts
}

fn push_doctor_attention_section(report: &DoctorReport, lines: &mut Vec<String>) {
    let actionable: Vec<_> = report
        .checks
        .iter()
        .filter(|check| check.status != DoctorStatus::Ok)
        .collect();
    if actionable.is_empty() {
        lines.push("ready: all checks passed".to_owned());
        return;
    }

    lines.push("needs attention:".to_owned());
    for check in actionable {
        push_doctor_check_lines(check, "- ", "  ", lines);
    }
}

fn push_doctor_check_lines(
    check: &DoctorCheck,
    header_prefix: &str,
    body_prefix: &str,
    lines: &mut Vec<String>,
) {
    lines.push(format!(
        "{header_prefix}[{}] {}",
        check.status.as_str(),
        check.name
    ));
    lines.push(format!("{body_prefix}{}", check.message));
    if let Some(remediation) = check.remediation.as_deref() {
        lines.push(format!("{body_prefix}fix: {remediation}"));
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/diagnostics_flow_tests.rs"]
mod tests;
