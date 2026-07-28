use std::collections::BTreeMap;

use sigil_kernel::CompactionFoldProtectionReason;

use super::{AppState, PaneFocus, TimelineRole, modal_flow::ModalState};
use crate::runner::{
    V2CompactionAdmission, V2CompactionApplySource, V2CompactionPreviewState, V2CompactionReview,
};

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

    pub(super) fn is_locally_prepared(&self) -> bool {
        matches!(
            self.review.admission,
            V2CompactionAdmission::Prepared { .. }
        )
    }

    pub(super) fn can_apply_standalone_shrink(&self) -> bool {
        matches!(
            self.review.admission,
            V2CompactionAdmission::Prepared {
                standalone_tool_output_shrink_available: true,
            }
        )
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
            format!("strategy: {}", self.review.strategy.as_str()),
            format!("fold: {} message(s)", plan.folded_event_ids.len()),
            format!("keep raw: {} message(s)", plan.retained_event_ids.len()),
            format!("protected: {protection_summary}"),
            format!("active boundary: {active_boundary}"),
        ];
        lines.push(match &self.review.admission {
            V2CompactionAdmission::Prepared { .. } => {
                "stage: local prepare only; no summary/provider request has been sent.".to_owned()
            }
            V2CompactionAdmission::Ready { .. } => {
                "stage: semantic summary generated and charged; activation is still pending."
                    .to_owned()
            }
            V2CompactionAdmission::Unavailable { .. } => {
                "stage: semantic preparation failed; the current epoch remains active.".to_owned()
            }
        });
        if let Some(adaptive) = &plan.adaptive_tail {
            lines.push(format!(
                "adaptive tail: fold {} complete turn(s) / {} tokens; keep {} complete turn(s) / {} raw token upper bound (target {} / effective {})",
                adaptive.folded_complete_turns,
                adaptive.folded_token_upper_bound,
                adaptive.retained_complete_turns,
                adaptive
                    .retained_token_upper_bound
                    .saturating_add(adaptive.protected_tail_token_upper_bound),
                adaptive.ordinary_target_tokens,
                adaptive.effective_target_tokens,
            ));
            lines.push(format!(
                "protected tail: {} event(s), {} token upper bound{}",
                adaptive.protected_tail_events.len(),
                adaptive.protected_tail_token_upper_bound,
                if adaptive.active_turn_extended {
                    " · active turn extended to exact-fit"
                } else {
                    ""
                },
            ));
        }
        if let Some(continuity) = &self.review.continuity {
            lines.push(format!(
                "continuity root: {}",
                continuity.root_objective.replace('\n', " ")
            ));
            lines.push(format!(
                "continuity evidence: {} active constraint(s) · {} authorization boundary item(s) · {} source ref(s)",
                continuity.active_constraint_count,
                continuity.authorization_boundary_count,
                continuity.source_ref_count,
            ));
            lines.extend(
                continuity
                    .active_constraints
                    .iter()
                    .take(8)
                    .map(|constraint| {
                        format!(
                            "  constraint: {} [{} · {}]",
                            constraint.text.replace('\n', " "),
                            constraint.source_event_id,
                            constraint.source_field_path,
                        )
                    }),
            );
            lines.push(format!(
                "continuity work: {} pending · {} unresolved · {} recoverable attachment(s)",
                continuity.pending_work_count,
                continuity.unresolved_question_count,
                continuity.recoverable_attachment_count,
            ));
        }
        lines.push(if self.review.native_carrier_requested {
            "native carrier: explicitly requested; exact-route capability is revalidated after portable apply"
                .to_owned()
        } else {
            "native carrier: disabled; portable checkpoint only".to_owned()
        });
        if self.review.tool_output_shrink_candidates.is_empty() {
            lines.push("next-epoch tool artifacts: none".to_owned());
        } else {
            lines.push(format!(
                "next-epoch tool artifacts: {} recoverable candidate(s)",
                self.review.tool_output_shrink_candidates.len()
            ));
            lines.extend(
                self.review
                    .tool_output_shrink_candidates
                    .iter()
                    .take(4)
                    .map(|candidate| {
                        format!(
                            "  {} [{}] {} · {} bytes / <= {} tokens · {} · {}",
                            candidate.tool_name,
                            candidate.tool_call_id,
                            candidate.status,
                            candidate.original_content_bytes,
                            candidate.original_content_token_upper_bound,
                            candidate.content_sha256,
                            candidate.artifact_ref,
                        )
                    }),
            );
            if let Some(candidate) = self.review.tool_output_shrink_candidates.first() {
                lines.push(format!(
                    "  head: {}",
                    bounded_preview(&candidate.head_excerpt, 160)
                ));
                lines.push(format!(
                    "  tail: {}",
                    bounded_preview(&candidate.tail_excerpt, 160)
                ));
                lines.push(format!(
                    "  reason: {} · {}",
                    candidate.reason, candidate.recovery_instruction
                ));
            }
        }
        match &self.review.admission {
            V2CompactionAdmission::Prepared {
                standalone_tool_output_shrink_available,
            } => {
                lines.push(
                    "full compaction: Enter generates one billed semantic summary".to_owned(),
                );
                if *standalone_tool_output_shrink_available {
                    lines.push(
                        "S clean large tool outputs only · Enter full compaction · Esc keep current"
                            .to_owned(),
                    );
                } else {
                    lines.push("Enter full compaction · Esc keep current".to_owned());
                }
            }
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
                summary_usage_observed,
                deterministic_emergency_fallback,
                summary_cache_read_tokens,
                summary_uncached_input_tokens,
                summary_output_tokens,
                summary_cost_nano_usd,
                economics_v2,
            } => {
                lines.push("target request: verified locally".to_owned());
                lines.push(format!(
                    "tokens: input {input_tokens} + output {output_tokens} + safety {safety_buffer_tokens} <= {context_window_tokens}"
                ));
                lines.push(format!(
                    "savings: {before_input_tokens} -> {input_tokens} ({savings_tokens} tokens, {} ppm; minimum {minimum_savings_tokens} tokens / {minimum_savings_ratio_ppm} ppm)",
                    savings_ratio_ppm,
                ));
                if *summary_usage_observed {
                    lines.push(format!(
                        "summary call: cache-read {summary_cache_read_tokens} · uncached {summary_uncached_input_tokens} · output {summary_output_tokens} tokens{}",
                        summary_cost_nano_usd.map_or_else(
                            String::new,
                            |cost| format!(" · {cost} nUSD"),
                        ),
                    ));
                } else {
                    lines.push(
                        "summary call: provider usage unavailable; token and cost totals are unknown"
                            .to_owned(),
                    );
                }
                if *deterministic_emergency_fallback {
                    lines.push(
                        "continuity: audited deterministic emergency floor (semantic narrative unavailable)"
                            .to_owned(),
                    );
                }
                if let Some(economics) = economics_v2 {
                    lines.push(format!(
                        "forecast: {:?} · confidence {:?} · admission {:?} ({:?})",
                        economics.forecast.pressure_state,
                        economics.forecast.input.expected_remaining_turns.confidence,
                        economics.admission.decision,
                        economics.admission.reason,
                    ));
                    if let Some(cost) = &economics.cost_projection {
                        lines.push(format!(
                            "cost horizon: keep {} nUSD · rotate {} nUSD · break-even {}",
                            cost.keep_cost_nano_usd,
                            cost.rotate_cost_nano_usd,
                            cost.break_even_turns
                                .map_or_else(|| "none".to_owned(), |turns| turns.to_string()),
                        ));
                    } else {
                        lines.push(
                            "cost horizon: unavailable; token heuristic remains manual-only"
                                .to_owned(),
                        );
                    }
                }
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

fn bounded_preview(value: &str, max_chars: usize) -> String {
    let mut chars = value.replace('\n', " ").chars().collect::<Vec<_>>();
    if chars.len() <= max_chars {
        return chars.into_iter().collect();
    }
    chars.truncate(max_chars);
    chars.into_iter().chain(std::iter::once('…')).collect()
}

impl AppState {
    pub(super) fn apply_v2_compaction_preview(&mut self, state: V2CompactionPreviewState) {
        let review = match state {
            V2CompactionPreviewState::NoFoldableHistory {
                durable_message_count,
                configured_tail_message_count,
            } => {
                let notice = format!(
                    "no newly foldable history: {durable_message_count} durable message(s); raw tail is {configured_tail_message_count}. Add completed turns or lower compaction.tail_messages."
                );
                self.last_notice = Some(notice.clone());
                self.push_timeline(TimelineRole::Notice, notice.clone());
                self.push_event("compact:preview", notice);
                return;
            }
            V2CompactionPreviewState::Review(review) => *review,
        };

        let fold_count = review.preview.plan.folded_event_ids.len();
        let keep_count = review.preview.plan.retained_event_ids.len();
        let admitted = matches!(&review.admission, V2CompactionAdmission::Ready { .. });
        let locally_prepared = matches!(&review.admission, V2CompactionAdmission::Prepared { .. });
        self.modal_state = Some(ModalState::V2CompactionPreview(Box::new(
            V2CompactionPreviewModalState { review },
        )));
        self.active_pane = PaneFocus::Activity;
        self.last_notice = Some(if admitted {
            "review V2 compaction; Enter applies the admitted checkpoint".to_owned()
        } else if locally_prepared {
            "review local compaction plan; Enter generates the billed summary, S cleans only large tool outputs"
                .to_owned()
        } else {
            "review V2 compaction; local target request admission is unavailable".to_owned()
        });
        self.push_event(
            "compact:preview",
            format!(
                "fold={fold_count} keep={keep_count} apply={}",
                if admitted {
                    "admitted"
                } else if locally_prepared {
                    "local_prepare"
                } else {
                    "unavailable"
                }
            ),
        );
    }

    pub(super) fn apply_standalone_tool_output_shrink(
        &mut self,
        context_epoch_id: String,
        projected_output_count: usize,
        entries: Vec<sigil_kernel::SessionLogEntry>,
    ) {
        self.sync_current_session_state(entries);
        let message = format!(
            "Large tool outputs cleaned for the next context epoch: {projected_output_count} output(s) ({context_epoch_id})"
        );
        self.push_timeline(TimelineRole::Notice, message.clone());
        self.push_event("compact:tool-output-shrink", message.clone());
        self.last_notice = Some(message);
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
        CompactionFoldProtectionReason::WholeTurnAtomicity => "whole-turn atomicity",
        CompactionFoldProtectionReason::ActiveToolOrApproval => "active tool/approval",
        CompactionFoldProtectionReason::OrphanTurnMessage => "orphan turn message",
    }
}
