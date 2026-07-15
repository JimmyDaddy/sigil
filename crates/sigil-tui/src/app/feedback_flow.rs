use std::{path::PathBuf, time::UNIX_EPOCH};

use anyhow::{Result, anyhow};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_runtime::support::{
    DoctorSupportProjectionContext, SupportBundleV1, SupportDoctorStatus, SupportEnvironmentV1,
    SupportPathKind, SupportPathRedaction, SupportRunPhase, SupportSessionProjectionContext,
    project_doctor_support_report_v1, project_support_session_summary_v1, write_support_bundle,
};

use super::{AppAction, AppState, ModalState, RunPhase};

pub(super) const GITHUB_BUG_REPORT_URL: &str =
    "https://github.com/JimmyDaddy/sigil/issues/new?template=bug-report.yml";

#[derive(Debug)]
pub(super) struct FeedbackModalState {
    bundle: SupportBundleV1,
    report_bytes: usize,
    exported_path: Option<PathBuf>,
    export_error: Option<String>,
}

impl FeedbackModalState {
    pub(super) fn lines(&self) -> Vec<String> {
        if let Some(path) = &self.exported_path {
            return vec![
                "Saved locally. Nothing was uploaded.".to_owned(),
                format!("Path: {}", path.display()),
                format!("Report issue: {GITHUB_BUG_REPORT_URL}"),
                "Review the JSON before attaching it to an issue.".to_owned(),
                String::new(),
                "C copy issue URL  Esc close".to_owned(),
            ];
        }

        let mut lines = vec![
            "Review before sharing. Nothing has been written or uploaded.".to_owned(),
            "Included: build, OS/architecture, doctor status and redacted checks.".to_owned(),
            format!(
                "Doctor: {} · {} checks · {} bytes",
                doctor_status_label(self.bundle.doctor.summary.overall_status),
                self.bundle.doctor.checks.len(),
                self.report_bytes
            ),
        ];
        if let Some(session) = &self.bundle.session {
            lines.push(format!(
                "Session: {} · {} durable entries · {}/{} · {}{}",
                session.session_id(),
                session.durable_entry_count(),
                session.provider(),
                session.model(),
                run_phase_label(session.run_phase()),
                if session.is_busy() { " · busy" } else { "" }
            ));
        }
        lines.extend([
            "Excluded: conversation, tool input/output, file content/diff, config file content,".to_owned(),
            "credential/environment values, local paths, private endpoints, and session-log content."
                .to_owned(),
            "Doctor metadata may include provider/model labels, MCP aliases, environment variable names,"
                .to_owned(),
            "and capability or sandbox status.".to_owned(),
        ]);
        if let Some(error) = &self.export_error {
            lines.push(String::new());
            lines.push(format!("Export failed: {error}"));
            lines.push("Enter retry  Esc close".to_owned());
        } else {
            lines.push(String::new());
            lines.push("Enter export locally  Esc cancel".to_owned());
        }
        lines
    }
}

impl AppState {
    pub(super) fn open_feedback_modal(&mut self) {
        match self.build_feedback_bundle() {
            Ok(bundle) => {
                let report_bytes = bundle
                    .to_pretty_json()
                    .expect("validated feedback bundle must remain serializable")
                    .len();
                self.modal_state = Some(ModalState::Feedback(Box::new(FeedbackModalState {
                    bundle,
                    report_bytes,
                    exported_path: None,
                    export_error: None,
                })));
                self.last_notice = Some("feedback report preview".to_owned());
            }
            Err(error) => {
                self.last_notice = Some(format!(
                    "feedback preview unavailable: {}",
                    bounded_safe_error(&error)
                ));
            }
        }
    }

    pub(super) fn feedback_modal_open(&self) -> bool {
        matches!(self.modal_state, Some(ModalState::Feedback(_)))
    }

    pub(super) fn handle_feedback_modal_key_event(&mut self, key: KeyEvent) -> Option<AppAction> {
        match key.code {
            KeyCode::Esc => {
                self.modal_state = None;
                self.last_notice = Some("closed feedback report".to_owned());
                None
            }
            KeyCode::Enter => {
                let cache_root = self.sigil_paths.cache_root.clone();
                let Some(ModalState::Feedback(state)) = self.modal_state.as_mut() else {
                    return None;
                };
                if state.exported_path.is_some() {
                    return None;
                }
                match write_support_bundle(&cache_root, &state.bundle) {
                    Ok(path) => {
                        state.exported_path = Some(path);
                        state.export_error = None;
                        self.last_notice = Some("feedback report saved locally".to_owned());
                    }
                    Err(error) => {
                        state.export_error = Some(bounded_safe_error(&error));
                        self.last_notice = Some("feedback report export failed".to_owned());
                    }
                }
                None
            }
            KeyCode::Char('c' | 'C')
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let exported = matches!(
                    self.modal_state,
                    Some(ModalState::Feedback(ref state)) if state.exported_path.is_some()
                );
                if exported {
                    self.last_notice = Some("copying feedback issue URL".to_owned());
                    Some(AppAction::CopyToClipboard {
                        text: GITHUB_BUG_REPORT_URL.to_owned(),
                    })
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn build_feedback_bundle(&self) -> Result<SupportBundleV1> {
        let generated_at_unix_ms = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_millis()
            .try_into()
            .map_err(|_| anyhow!("system time does not fit support timestamp"))?;
        let environment = SupportEnvironmentV1::current();
        let path_redactions = [
            SupportPathRedaction::new(&self.config_path, SupportPathKind::Config),
            SupportPathRedaction::new(&self.workspace_root, SupportPathKind::Workspace),
            SupportPathRedaction::new(&self.sigil_paths.cache_root, SupportPathKind::Cache),
            SupportPathRedaction::new(&self.sigil_paths.state_root, SupportPathKind::State),
        ];
        let doctor = project_doctor_support_report_v1(
            &self.build_tui_doctor_report(),
            DoctorSupportProjectionContext {
                generated_at_unix_ms,
                build: &self.support_build_info,
                environment: &environment,
                redactor: &self.secret_redactor,
                path_redactions: &path_redactions,
            },
        )?;
        let session = project_support_session_summary_v1(
            &self.session_id,
            self.session_browser.current_entries.len(),
            &self.runtime.provider_name,
            &self.runtime.model_name,
            support_run_phase(&self.runtime.run_phase),
            self.runtime.is_busy,
            SupportSessionProjectionContext {
                redactor: &self.secret_redactor,
                path_redactions: &path_redactions,
            },
        )?;
        Ok(SupportBundleV1::new(doctor, Some(session)))
    }
}

fn support_run_phase(phase: &RunPhase) -> SupportRunPhase {
    match phase {
        RunPhase::Idle => SupportRunPhase::Idle,
        RunPhase::Thinking => SupportRunPhase::Thinking,
        RunPhase::Agent(_) => SupportRunPhase::Agent,
        RunPhase::Tool(_) => SupportRunPhase::Tool,
        RunPhase::Streaming => SupportRunPhase::Streaming,
    }
}

fn doctor_status_label(status: SupportDoctorStatus) -> &'static str {
    match status {
        SupportDoctorStatus::Ok => "ok",
        SupportDoctorStatus::Warn => "warn",
        SupportDoctorStatus::Error => "error",
    }
}

fn run_phase_label(phase: SupportRunPhase) -> &'static str {
    match phase {
        SupportRunPhase::Idle => "idle",
        SupportRunPhase::Thinking => "thinking",
        SupportRunPhase::Agent => "agent",
        SupportRunPhase::Tool => "tool",
        SupportRunPhase::Streaming => "streaming",
    }
}

fn bounded_safe_error(error: &anyhow::Error) -> String {
    let safe = sigil_kernel::safe_persistence_text(&error.to_string());
    safe.chars().take(240).collect()
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/feedback_flow_tests.rs"]
mod tests;
