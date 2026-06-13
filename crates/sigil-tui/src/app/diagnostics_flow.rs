use sigil_runtime::doctor::{DoctorReport, build_doctor_report};

use super::{AppState, TimelineRole};

impl AppState {
    pub(super) fn show_doctor_report(&mut self) {
        let report = build_doctor_report(&self.config_path, &self.workspace_root);
        let status = report.overall_status().as_str();
        self.last_notice = Some(format!("doctor: {status}"));
        self.push_event("doctor", status);
        self.push_timeline(TimelineRole::Notice, render_doctor_report(&report));
    }
}

fn render_doctor_report(report: &DoctorReport) -> String {
    let mut lines = vec![format!("doctor: {}", report.overall_status().as_str())];
    for check in &report.checks {
        lines.push(format!(
            "[{}] {} - {}",
            check.status.as_str(),
            check.name,
            check.message
        ));
        if let Some(remediation) = check.remediation.as_deref() {
            lines.push(format!("    fix: {remediation}"));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
#[path = "tests/diagnostics_flow_tests.rs"]
mod tests;
