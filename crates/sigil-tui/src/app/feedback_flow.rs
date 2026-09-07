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
const MAX_FEEDBACK_REVIEW_PAGE_LINES: usize = 18;
const FEEDBACK_REVIEW_RESERVED_TERMINAL_ROWS: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FeedbackModalView {
    Summary,
    JsonReview,
}

#[derive(Debug)]
pub(super) struct FeedbackModalState {
    bundle: SupportBundleV1,
    report_json: String,
    report_bytes: usize,
    exported_path: Option<PathBuf>,
    export_error: Option<String>,
    view: FeedbackModalView,
    review_offset: usize,
}

impl FeedbackModalState {
    pub(super) fn lines(&self, terminal_height: u16) -> Vec<String> {
        if self.view == FeedbackModalView::JsonReview && self.exported_path.is_some() {
            return self.json_review_lines(feedback_review_page_lines(terminal_height));
        }
        if let Some(path) = &self.exported_path {
            let directory = path.parent().unwrap_or(path);
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("support report");
            return vec![
                "Saved locally. Nothing was uploaded.".to_owned(),
                format!("Folder: {}", directory.display()),
                format!("File: {file_name}"),
                "Review the JSON before sharing it. Then open the form and attach the file yourself."
                    .to_owned(),
                String::new(),
                "Enter review JSON  O reveal file  B open bug form".to_owned(),
                "C copy report path  U copy issue URL  Esc close".to_owned(),
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
            "credential/environment names and values, local paths, private endpoints, and session-log content."
                .to_owned(),
            "Metadata may include provider/model labels, MCP aliases, and capability or sandbox status."
                .to_owned(),
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

    fn json_review_lines(&self, page_lines: usize) -> Vec<String> {
        let report_lines = self.report_json.lines().collect::<Vec<_>>();
        let total = report_lines.len();
        let start = self.review_offset.min(total.saturating_sub(page_lines));
        let end = start.saturating_add(page_lines).min(total);
        let mut lines = vec![
            "Reviewing the exact redacted JSON saved locally. Nothing was uploaded.".to_owned(),
            "Up/Down or J/K scroll  PgUp/PgDn page  Home/End jump  Esc back".to_owned(),
            format!("Lines: {}-{} of {total}", start.saturating_add(1), end),
            String::new(),
        ];
        lines.extend(
            report_lines[start..end]
                .iter()
                .map(|line| (*line).to_owned()),
        );
        lines
    }

    fn scroll_review_by(&mut self, delta: isize, page_lines: usize) {
        let max_offset = self.report_json.lines().count().saturating_sub(page_lines);
        self.review_offset = self
            .review_offset
            .saturating_add_signed(delta)
            .min(max_offset);
    }
}

impl AppState {
    pub(super) fn open_feedback_modal(&mut self) {
        match self.build_feedback_bundle() {
            Ok(bundle) => {
                let report_json = bundle
                    .to_pretty_json()
                    .expect("validated feedback bundle must remain serializable");
                let report_bytes = report_json.len();
                self.modal_state = Some(ModalState::Feedback(Box::new(FeedbackModalState {
                    bundle,
                    report_json,
                    report_bytes,
                    exported_path: None,
                    export_error: None,
                    view: FeedbackModalView::Summary,
                    review_offset: 0,
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
        let review_page_lines = feedback_review_page_lines(self.terminal_height);
        match key.code {
            KeyCode::Esc => {
                if let Some(ModalState::Feedback(state)) = self.modal_state.as_mut()
                    && state.view == FeedbackModalView::JsonReview
                {
                    state.view = FeedbackModalView::Summary;
                    self.last_notice = Some("closed feedback JSON review".to_owned());
                    return None;
                }
                self.modal_state = None;
                self.last_notice = Some("closed feedback report".to_owned());
                None
            }
            KeyCode::Enter | KeyCode::Char('\n') | KeyCode::Char('\r') => {
                let cache_root = self.sigil_paths.cache_root.clone();
                let Some(ModalState::Feedback(state)) = self.modal_state.as_mut() else {
                    return None;
                };
                if state.exported_path.is_some() {
                    if state.view == FeedbackModalView::Summary {
                        state.view = FeedbackModalView::JsonReview;
                        state.review_offset = 0;
                        self.last_notice = Some("reviewing redacted feedback JSON".to_owned());
                    }
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
            KeyCode::Up | KeyCode::Char('k' | 'K') => {
                if let Some(ModalState::Feedback(state)) = self.modal_state.as_mut()
                    && state.view == FeedbackModalView::JsonReview
                {
                    state.scroll_review_by(-1, review_page_lines);
                }
                None
            }
            KeyCode::Down | KeyCode::Char('j' | 'J') => {
                if let Some(ModalState::Feedback(state)) = self.modal_state.as_mut()
                    && state.view == FeedbackModalView::JsonReview
                {
                    state.scroll_review_by(1, review_page_lines);
                }
                None
            }
            KeyCode::PageUp => {
                if let Some(ModalState::Feedback(state)) = self.modal_state.as_mut()
                    && state.view == FeedbackModalView::JsonReview
                {
                    state.scroll_review_by(-(review_page_lines as isize), review_page_lines);
                }
                None
            }
            KeyCode::PageDown => {
                if let Some(ModalState::Feedback(state)) = self.modal_state.as_mut()
                    && state.view == FeedbackModalView::JsonReview
                {
                    state.scroll_review_by(review_page_lines as isize, review_page_lines);
                }
                None
            }
            KeyCode::Home => {
                if let Some(ModalState::Feedback(state)) = self.modal_state.as_mut()
                    && state.view == FeedbackModalView::JsonReview
                {
                    state.review_offset = 0;
                }
                None
            }
            KeyCode::End => {
                if let Some(ModalState::Feedback(state)) = self.modal_state.as_mut()
                    && state.view == FeedbackModalView::JsonReview
                {
                    state.review_offset = state
                        .report_json
                        .lines()
                        .count()
                        .saturating_sub(review_page_lines);
                }
                None
            }
            KeyCode::Char('c' | 'C')
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let path = match self.modal_state.as_ref() {
                    Some(ModalState::Feedback(state))
                        if state.view == FeedbackModalView::Summary =>
                    {
                        state.exported_path.as_ref()?.to_string_lossy().into_owned()
                    }
                    _ => return None,
                };
                self.last_notice = Some("copying feedback report path".to_owned());
                Some(AppAction::CopyToClipboard { text: path })
            }
            KeyCode::Char('u' | 'U')
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let exported = matches!(
                    self.modal_state,
                    Some(ModalState::Feedback(ref state))
                        if state.exported_path.is_some()
                            && state.view == FeedbackModalView::Summary
                );
                exported.then(|| AppAction::CopyToClipboard {
                    text: GITHUB_BUG_REPORT_URL.to_owned(),
                })
            }
            KeyCode::Char('b' | 'B')
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let exported = matches!(
                    self.modal_state,
                    Some(ModalState::Feedback(ref state))
                        if state.exported_path.is_some()
                            && state.view == FeedbackModalView::Summary
                );
                exported.then(|| AppAction::OpenExternalUrl {
                    url: GITHUB_BUG_REPORT_URL.to_owned(),
                })
            }
            KeyCode::Char('o' | 'O')
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let path = match self.modal_state.as_ref() {
                    Some(ModalState::Feedback(state))
                        if state.view == FeedbackModalView::Summary =>
                    {
                        state.exported_path.clone()?
                    }
                    _ => return None,
                };
                Some(AppAction::RevealFile { path })
            }
            _ => None,
        }
    }

    pub(crate) fn record_feedback_external_action_success(&mut self, notice: &str) {
        self.last_notice = Some(notice.to_owned());
    }

    pub(crate) fn record_feedback_external_action_failure(
        &mut self,
        action: &str,
        error: &anyhow::Error,
    ) {
        self.last_notice = Some(format!("{action} failed: {}", bounded_safe_error(error)));
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

fn feedback_review_page_lines(terminal_height: u16) -> usize {
    usize::from(terminal_height)
        .saturating_sub(FEEDBACK_REVIEW_RESERVED_TERMINAL_ROWS)
        .clamp(1, MAX_FEEDBACK_REVIEW_PAGE_LINES)
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
