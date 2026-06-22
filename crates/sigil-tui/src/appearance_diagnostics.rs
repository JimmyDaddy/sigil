use sigil_kernel::AppearanceConfig;
use sigil_runtime::doctor::{DoctorCheck, DoctorStatus};

use crate::ui::theme::diagnostics::{
    ThemeDiagnostic, ThemeDiagnosticError, ThemeDiagnosticKind, ThemeDiagnosticMetric,
    ThemeDiagnosticReport, ThemeDiagnosticTier, diagnose_appearance,
};

/// Builds TUI appearance checks for the shared doctor report.
#[must_use]
pub fn appearance_doctor_checks(appearance: &AppearanceConfig) -> Vec<DoctorCheck> {
    match diagnose_appearance(appearance) {
        Ok(report) => checks_from_report(report),
        Err(error) => vec![check_from_error(error)],
    }
}

fn checks_from_report(report: ThemeDiagnosticReport) -> Vec<DoctorCheck> {
    if report.diagnostics.is_empty() {
        return vec![DoctorCheck {
            status: DoctorStatus::Ok,
            name: "appearance:contrast".to_owned(),
            message: format!(
                "theme={} overrides={} checked={}",
                report.theme_id.as_str(),
                report.override_count,
                report.checked
            ),
            remediation: None,
        }];
    }

    let mut checks = Vec::new();
    let mut semantic = Vec::new();
    let mut structural = Vec::new();
    for diagnostic in report.diagnostics {
        match diagnostic.tier {
            ThemeDiagnosticTier::Semantic => semantic.push(diagnostic),
            ThemeDiagnosticTier::Structural => structural.push(diagnostic),
            _ => checks.push(check_from_diagnostic(&diagnostic)),
        }
    }
    if !semantic.is_empty() {
        checks.push(grouped_check(
            "appearance:semantic",
            &semantic,
            "state color pairs are too similar",
            "adjust the listed [appearance.colors] tokens so status and risk states remain visually distinct",
        ));
    }
    if !structural.is_empty() {
        checks.push(grouped_check(
            "appearance:structural",
            &structural,
            "structural cues are weak",
            "adjust border or background override tokens so focus, borders, and dividers remain visible",
        ));
    }
    checks
}

fn check_from_error(error: ThemeDiagnosticError) -> DoctorCheck {
    DoctorCheck {
        status: DoctorStatus::Warn,
        name: "appearance:colors".to_owned(),
        message: error.message().to_owned(),
        remediation: Some(
            "update or remove invalid [appearance.colors] entries, then run /config Appearance to preview"
                .to_owned(),
        ),
    }
}

fn check_from_diagnostic(diagnostic: &ThemeDiagnostic) -> DoctorCheck {
    DoctorCheck {
        status: DoctorStatus::Warn,
        name: format!(
            "{}:{}",
            check_prefix_for_kind(diagnostic.kind),
            diagnostic.name
        ),
        message: format!(
            "{} is below {} ({})",
            diagnostic_summary(diagnostic),
            format_metric(diagnostic.minimum, diagnostic.metric),
            tier_label(diagnostic.tier),
        ),
        remediation: Some(format!(
            "{}; run /config Appearance to preview",
            diagnostic.remediation
        )),
    }
}

fn grouped_check(
    name: &str,
    diagnostics: &[ThemeDiagnostic],
    label: &str,
    remediation: &str,
) -> DoctorCheck {
    DoctorCheck {
        status: DoctorStatus::Warn,
        name: name.to_owned(),
        message: format!(
            "{} {}: {}",
            diagnostics.len(),
            label,
            diagnostics
                .iter()
                .take(3)
                .map(grouped_diagnostic_summary)
                .collect::<Vec<_>>()
                .join("; ")
        ),
        remediation: Some(format!("{remediation}; run /config Appearance to preview")),
    }
}

fn diagnostic_summary(diagnostic: &ThemeDiagnostic) -> String {
    match diagnostic.metric {
        ThemeDiagnosticMetric::ContrastRatio => format!(
            "{} on {} contrast {}",
            diagnostic.tokens[0],
            diagnostic.tokens[1],
            format_metric(diagnostic.actual, diagnostic.metric)
        ),
        ThemeDiagnosticMetric::SrgbDistance => format!(
            "{}/{} distance {}",
            diagnostic.tokens[0],
            diagnostic.tokens[1],
            format_metric(diagnostic.actual, diagnostic.metric)
        ),
    }
}

fn grouped_diagnostic_summary(diagnostic: &ThemeDiagnostic) -> String {
    format!(
        "{}/{} {} < {}",
        diagnostic.tokens[0],
        diagnostic.tokens[1],
        format_metric(diagnostic.actual, diagnostic.metric),
        format_metric(diagnostic.minimum, diagnostic.metric)
    )
}

fn format_metric(value: f32, metric: ThemeDiagnosticMetric) -> String {
    match metric {
        ThemeDiagnosticMetric::ContrastRatio => format!("{value:.1}"),
        ThemeDiagnosticMetric::SrgbDistance => format!("{value:.2}"),
    }
}

fn check_prefix_for_kind(kind: ThemeDiagnosticKind) -> &'static str {
    match kind {
        ThemeDiagnosticKind::ContrastPair => "appearance:contrast",
        ThemeDiagnosticKind::SemanticSeparation => "appearance:semantic",
        ThemeDiagnosticKind::StructuralCue => "appearance:structural",
    }
}

fn tier_label(tier: ThemeDiagnosticTier) -> &'static str {
    match tier {
        ThemeDiagnosticTier::Safety => "safety surface",
        ThemeDiagnosticTier::Core => "readability",
        ThemeDiagnosticTier::Surface => "surface readability",
        ThemeDiagnosticTier::Semantic => "state colors too similar",
        ThemeDiagnosticTier::Structural => "structural cue",
        ThemeDiagnosticTier::Advisory => "muted text",
    }
}

#[cfg(test)]
#[path = "tests/appearance_diagnostics_tests.rs"]
mod tests;
