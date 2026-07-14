use std::collections::BTreeMap;

use sigil_kernel::CompactionFoldProtectionReason;

use super::{AppState, PaneFocus, TimelineRole, modal_flow::ModalState};
use crate::runner::{V2CompactionAdmission, V2CompactionApplySource, V2CompactionReview};

#[derive(Debug)]
pub(super) struct V2CompactionPreviewModalState {
    review: V2CompactionReview,
}

impl V2CompactionPreviewModalState {
    pub(super) fn request_id(&self) -> u64 {
        self.review.request_id
    }

    pub(super) fn is_admitted(&self) -> bool {
        matches!(self.review.admission, V2CompactionAdmission::Ready { .. })
    }

    pub(super) fn lines(&self) -> Vec<String> {
        let plan = &self.review.preview.plan;
        let mut protections = BTreeMap::<&str, usize>::new();
        for protected in &plan.protected_events {
            *protections
                .entry(protection_reason_label(&protected.reason))
                .or_default() += 1;
        }
        let protection_summary = if protections.is_empty() {
            "none".to_owned()
        } else {
            protections
                .into_iter()
                .map(|(reason, count)| format!("{count} {reason}"))
                .collect::<Vec<_>>()
                .join(" · ")
        };
        let active_boundary = self
            .review
            .preview
            .active_compaction_id
            .as_deref()
            .unwrap_or("none");
        let mut lines = vec![
            "Review — no session data has been changed yet.".to_owned(),
            "strategy: portable semantic checkpoint".to_owned(),
            format!("fold: {} message(s)", plan.folded_event_ids.len()),
            format!("keep raw: {} message(s)", plan.retained_event_ids.len()),
            format!("protected: {protection_summary}"),
            format!("active boundary: {active_boundary}"),
            "risk: no checkpoint, summary, or provider request has been created.".to_owned(),
        ];
        match &self.review.admission {
            V2CompactionAdmission::Ready {
                before_input_tokens,
                input_tokens,
                context_window_tokens,
                output_tokens,
                safety_buffer_tokens,
                savings_tokens,
                savings_ratio_ppm,
                minimum_savings_tokens,
                minimum_savings_ratio_ppm,
            } => {
                lines.push("target request: verified locally".to_owned());
                lines.push(format!(
                    "tokens: input {input_tokens} + output {output_tokens} + safety {safety_buffer_tokens} <= {context_window_tokens}"
                ));
                lines.push(format!(
                    "savings: {before_input_tokens} -> {input_tokens} ({savings_tokens} tokens, {} ppm; minimum {minimum_savings_tokens} tokens / {minimum_savings_ratio_ppm} ppm)",
                    savings_ratio_ppm,
                ));
                lines.push("Enter apply  Esc cancel".to_owned());
            }
            V2CompactionAdmission::Unavailable { reason } => {
                lines.push("target request: unavailable".to_owned());
                lines.push(format!("apply: unavailable — {reason}"));
                lines.push("Enter/Esc close".to_owned());
            }
        }
        lines
    }
}

impl AppState {
    pub(super) fn apply_v2_compaction_preview(&mut self, review: Option<V2CompactionReview>) {
        let Some(review) = review else {
            let notice = "no newly foldable history for V2 compaction".to_owned();
            self.last_notice = Some(notice.clone());
            self.push_timeline(TimelineRole::Notice, notice.clone());
            self.push_event("compact:preview", notice);
            return;
        };

        let fold_count = review.preview.plan.folded_event_ids.len();
        let keep_count = review.preview.plan.retained_event_ids.len();
        let admitted = matches!(review.admission, V2CompactionAdmission::Ready { .. });
        self.modal_state = Some(ModalState::V2CompactionPreview(Box::new(
            V2CompactionPreviewModalState { review },
        )));
        self.active_pane = PaneFocus::Activity;
        self.last_notice = Some(if admitted {
            "review V2 compaction; Enter applies the admitted checkpoint".to_owned()
        } else {
            "review V2 compaction; local target request admission is unavailable".to_owned()
        });
        self.push_event(
            "compact:preview",
            format!(
                "fold={fold_count} keep={keep_count} apply={}",
                if admitted { "admitted" } else { "unavailable" }
            ),
        );
    }

    pub(super) fn apply_v2_compaction_applied(
        &mut self,
        source: V2CompactionApplySource,
        compaction_id: String,
        folded_event_count: usize,
        entries: Vec<sigil_kernel::SessionLogEntry>,
    ) {
        self.sync_current_session_state(entries);
        let prefix = match source {
            V2CompactionApplySource::ManualConfirmation => "Context compacted",
            V2CompactionApplySource::IdleAutomatic => "Context compacted automatically",
            V2CompactionApplySource::PreTurnPressure => {
                "Context compacted before dispatching the queued follow-up"
            }
            V2CompactionApplySource::OverflowRecovery => {
                "Context compacted after a context-window rejection"
            }
        };
        let message = format!("{prefix}: {folded_event_count} message(s) folded ({compaction_id})");
        self.push_timeline(TimelineRole::Notice, message.clone());
        self.push_event("compact:applied", message.clone());
        self.last_notice = Some(message);
    }

    pub(super) fn apply_v2_compaction_failed(&mut self, error: String) {
        self.last_notice = Some(format!("V2 compaction was not applied: {error}"));
        self.push_timeline(TimelineRole::Notice, "V2 compaction was not applied");
        self.push_event("compact:apply-error", error);
    }
}

fn protection_reason_label(reason: &CompactionFoldProtectionReason) -> &'static str {
    match reason {
        CompactionFoldProtectionReason::ExistingCompactionBoundary => "existing boundary",
        CompactionFoldProtectionReason::ControlState => "control state",
        CompactionFoldProtectionReason::NonMessageDurableEvent => "non-message event",
        CompactionFoldProtectionReason::MalformedMessage => "malformed message",
        CompactionFoldProtectionReason::UnsafeToolPair => "unsafe tool pair",
        CompactionFoldProtectionReason::UnpairedToolResult => "unpaired tool result",
    }
}
