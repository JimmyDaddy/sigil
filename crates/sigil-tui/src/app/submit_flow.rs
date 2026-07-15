use std::path::Path;

use anyhow::Result;
use sigil_kernel::{SkillDescriptor, SkillRunMode, SkillTrustState};

use super::session_flow::current_focus_label;
use super::{AppAction, AppState, ComposerMode, PaneFocus, RunPhase, TimelineRole};
use crate::slash::ResolvedSlashCommand;

impl AppState {
    pub fn submit_input(&mut self) -> Result<Option<AppAction>> {
        let prompt = self.composer.input.trim().to_owned();
        if prompt.is_empty() {
            return Ok(None);
        }
        self.discard_cleared_input_draft();
        self.record_input_history(prompt.clone());
        self.reset_input_history_navigation();

        if self.composer.queue_edit_target.is_some() {
            return Ok(self.finish_queue_edit_submission(prompt));
        }

        if prompt.starts_with('/') {
            let Some(command) = self.resolve_slash_command(&prompt) else {
                self.push_timeline(TimelineRole::Notice, "unknown slash command");
                self.push_event(
                    "slash:unknown",
                    sigil_kernel::safe_persistence_text(&prompt),
                );
                self.last_notice = Some("unknown slash command".to_owned());
                return Ok(None);
            };

            return self.execute_slash_command(command, prompt);
        }

        if prompt.trim_start().starts_with('@') {
            if self.runtime.is_busy {
                self.push_timeline(TimelineRole::Notice, "busy; @agent input kept for later");
                self.push_event("agent:busy", sigil_kernel::safe_persistence_text(&prompt));
                self.last_notice = Some("busy; @agent input kept for later".to_owned());
                return Ok(None);
            }
            let (profile_id, agent_prompt) = match self.resolve_agent_mention_invocation(&prompt) {
                Ok(invocation) => invocation,
                Err(error) => {
                    let notice = error.to_string();
                    self.push_timeline(TimelineRole::Notice, notice.clone());
                    self.push_event(
                        "agent:unknown",
                        sigil_kernel::safe_persistence_text(&prompt),
                    );
                    self.last_notice = Some(notice);
                    return Ok(None);
                }
            };
            return Ok(Some(self.start_agent_profile_invocation(
                profile_id,
                agent_prompt,
                prompt,
            )));
        }

        if self.runtime.is_busy {
            let (kind, target) = self.active_conversation_queue_submission();
            self.push_optimistic_conversation_queue_item(prompt.clone(), kind, target.clone());
            self.composer.input.clear();
            self.composer.input_cursor = 0;
            self.composer.input_paste_spans.clear();
            self.reset_slash_selector();
            self.last_notice = Some("follow-up will run next".to_owned());
            return Ok(Some(AppAction::QueueConversationInput {
                prompt,
                kind,
                target,
            }));
        }

        self.clear_pending_plan_approval();

        if self.composer.mode == ComposerMode::Plan {
            self.composer.input.clear();
            self.composer.input_cursor = 0;
            self.composer.input_paste_spans.clear();
            self.reset_slash_selector();
            self.timeline_scroll_back = 0;
            let safe_prompt = sigil_kernel::safe_persistence_text(&prompt);
            self.push_timeline(TimelineRole::User, safe_prompt.clone());
            self.push_event("input", format!("submitted plan prompt {safe_prompt}"));
            self.active_pane = PaneFocus::Composer;
            self.push_event("focus", current_focus_label(self));
            self.runtime.is_busy = true;
            self.runtime.run_phase = RunPhase::Thinking;
            self.last_notice = Some(ComposerMode::Plan.notice().to_owned());
            self.runtime.last_phase_marker = None;
            self.push_phase_marker(format!(
                "{}|{}",
                ComposerMode::Plan.phase_marker(),
                self.runtime.model_name
            ));
            self.streaming_assistant_index = None;
            self.streaming_reasoning_index = None;
            self.composer.mode = ComposerMode::Build;
            self.refresh_usage_sidebar_cache();
            return Ok(Some(AppAction::SubmitPlanPrompt(prompt)));
        }

        self.composer.input.clear();
        self.composer.input_cursor = 0;
        self.composer.input_paste_spans.clear();
        self.reset_slash_selector();
        self.timeline_scroll_back = 0;
        let safe_prompt = sigil_kernel::safe_persistence_text(&prompt);
        self.push_timeline(TimelineRole::User, safe_prompt.clone());
        self.push_event("input", format!("submitted {safe_prompt}"));
        self.active_pane = PaneFocus::Composer;
        self.push_event("focus", current_focus_label(self));
        self.runtime.is_busy = true;
        self.runtime.run_phase = RunPhase::Thinking;
        self.last_notice = Some("thinking".to_owned());
        self.runtime.last_phase_marker = None;
        self.push_phase_marker(format!("thinking|{}", self.runtime.model_name));
        self.streaming_assistant_index = None;
        self.streaming_reasoning_index = None;
        self.refresh_usage_sidebar_cache();
        Ok(Some(AppAction::SubmitPrompt(prompt)))
    }

    pub(super) fn execute_slash_command(
        &mut self,
        command: ResolvedSlashCommand,
        prompt: String,
    ) -> Result<Option<AppAction>> {
        self.composer.input.clear();
        self.composer.input_cursor = 0;
        self.composer.input_paste_spans.clear();
        self.pending_mouse_slash_confirmation = None;
        self.reset_slash_selector();
        if command.canonical != "/feedback" {
            self.push_event("slash", sigil_kernel::safe_persistence_text(&prompt));
        }
        match command.canonical.as_str() {
            "/compact" => {
                if self.runtime.is_busy {
                    self.push_timeline(TimelineRole::Notice, "busy; preview compact later");
                    Ok(None)
                } else {
                    self.last_notice = Some("V2 compact preview requested".to_owned());
                    Ok(Some(AppAction::PreviewV2Compaction))
                }
            }
            "/config" => {
                self.open_config_panel();
                Ok(None)
            }
            "/doctor" => {
                self.show_doctor_report();
                Ok(None)
            }
            "/feedback" => {
                self.open_feedback_modal();
                Ok(None)
            }
            "@agent" => self.execute_agent_slash_command(&command, &prompt),
            "/agent" => self.activate_agent_from_command(&command.arg),
            "/effort" => self.set_runtime_reasoning_effort_from_command(&command.arg),
            "/model" => self.set_runtime_model_from_command(&command.arg),
            "/queue" => self.execute_queue_slash_command(&command.arg),
            "/new" => {
                if self.runtime.is_busy {
                    self.push_timeline(TimelineRole::Notice, "busy; start new session later");
                    return Ok(None);
                }
                let session_log_path = self.new_session_log_path();
                Ok(Some(AppAction::StartNewSession { session_log_path }))
            }
            "/plan" => self.execute_plan_slash_command(command.arg.trim()),
            "/task" => self.execute_task_slash_command(command.arg.trim()),
            "/quit" => {
                self.should_quit = true;
                self.push_timeline(TimelineRole::Notice, "quitting");
                Ok(None)
            }
            "/resume" => self.execute_resume_slash_command(&command.arg),
            _ => {
                if let Some(action) = self.execute_skill_slash_command(&command, &prompt)? {
                    return Ok(Some(action));
                }
                self.push_timeline(TimelineRole::Notice, "unknown slash command");
                Ok(None)
            }
        }
    }

    fn execute_plan_slash_command(&mut self, arg: &str) -> Result<Option<AppAction>> {
        if self.runtime.is_busy {
            self.push_timeline(TimelineRole::Notice, "busy; plan later");
            return Ok(None);
        }
        if arg.is_empty() {
            self.composer.input.clear();
            self.composer.input_cursor = 0;
            self.composer.input_paste_spans.clear();
            self.reset_slash_selector();
            self.composer.mode = ComposerMode::Plan;
            self.last_notice = Some("plan mode".to_owned());
            self.push_event("mode", "plan");
            return Ok(None);
        }
        if arg == "continue" || arg.starts_with("continue ") {
            self.push_timeline(
                TimelineRole::Notice,
                "plan mode cannot continue durable tasks; use /task continue",
            );
            self.last_notice = Some("use /task continue".to_owned());
            return Ok(None);
        }

        let plan_prompt = arg.to_owned();
        let safe_plan_prompt = sigil_kernel::safe_persistence_text(&plan_prompt);
        self.clear_pending_plan_approval();
        self.composer.input.clear();
        self.composer.input_cursor = 0;
        self.composer.input_paste_spans.clear();
        self.reset_slash_selector();
        self.timeline_scroll_back = 0;
        self.push_timeline(TimelineRole::User, format!("/plan {safe_plan_prompt}"));
        self.push_event("input", format!("submitted plan prompt {safe_plan_prompt}"));
        self.active_pane = PaneFocus::Composer;
        self.push_event("focus", current_focus_label(self));
        self.runtime.is_busy = true;
        self.runtime.run_phase = RunPhase::Thinking;
        self.last_notice = Some(ComposerMode::Plan.notice().to_owned());
        self.runtime.last_phase_marker = None;
        self.push_phase_marker(format!(
            "{}|{}",
            ComposerMode::Plan.phase_marker(),
            self.runtime.model_name
        ));
        self.streaming_assistant_index = None;
        self.streaming_reasoning_index = None;
        self.refresh_usage_sidebar_cache();
        Ok(Some(AppAction::SubmitPlanPrompt(plan_prompt)))
    }

    fn execute_task_slash_command(&mut self, arg: &str) -> Result<Option<AppAction>> {
        if self.runtime.is_busy {
            self.push_timeline(TimelineRole::Notice, "busy; task later");
            return Ok(None);
        }
        if arg.is_empty() {
            self.push_timeline(TimelineRole::Notice, "usage: /task <task|continue>");
            self.last_notice = Some("usage: /task <task|continue>".to_owned());
            return Ok(None);
        }
        if arg == "continue" || arg.starts_with("continue ") {
            let guidance = arg
                .strip_prefix("continue")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned);
            self.runtime.is_busy = true;
            self.runtime.run_phase = RunPhase::Thinking;
            self.last_notice = Some("continuing task".to_owned());
            self.runtime.last_phase_marker = None;
            self.push_phase_marker(format!("task|{}", self.runtime.model_name));
            self.streaming_assistant_index = None;
            self.streaming_reasoning_index = None;
            self.refresh_usage_sidebar_cache();
            return Ok(Some(AppAction::ContinueTask {
                task_id: None,
                guidance,
            }));
        }

        let objective = arg.to_owned();
        let safe_objective = sigil_kernel::safe_persistence_text(&objective);
        self.clear_pending_plan_approval();
        self.timeline_scroll_back = 0;
        self.push_timeline(TimelineRole::User, format!("/task {safe_objective}"));
        self.push_event("input", format!("submitted task {safe_objective}"));
        self.active_pane = PaneFocus::Composer;
        self.push_event("focus", current_focus_label(self));
        self.runtime.is_busy = true;
        self.runtime.run_phase = RunPhase::Thinking;
        self.last_notice = Some("planning task".to_owned());
        self.runtime.last_phase_marker = None;
        self.push_phase_marker(format!("task|{}", self.runtime.model_name));
        self.streaming_assistant_index = None;
        self.streaming_reasoning_index = None;
        self.refresh_usage_sidebar_cache();
        Ok(Some(AppAction::SubmitTask(objective)))
    }

    fn execute_resume_slash_command(&mut self, arg: &str) -> Result<Option<AppAction>> {
        if self.runtime.is_busy {
            self.push_timeline(TimelineRole::Notice, "busy; resume later");
            return Ok(None);
        }

        self.refresh_session_history();
        match self.resolve_resume_target(arg) {
            Some(path) => Ok(Some(AppAction::SwitchSession {
                session_log_path: path,
            })),
            None => {
                self.push_timeline(TimelineRole::Notice, "no matching session");
                Ok(None)
            }
        }
    }

    fn execute_agent_slash_command(
        &mut self,
        command: &ResolvedSlashCommand,
        prompt: &str,
    ) -> Result<Option<AppAction>> {
        if self.runtime.is_busy {
            self.push_timeline(TimelineRole::Notice, "busy; invoke agent later");
            self.last_notice = Some("busy; invoke agent later".to_owned());
            return Ok(None);
        }
        let Some((profile_id, agent_prompt)) =
            command.arg.trim_start().split_once(char::is_whitespace)
        else {
            self.push_timeline(TimelineRole::Notice, "usage: /agent-name <prompt>");
            self.last_notice = Some("usage: /agent-name <prompt>".to_owned());
            return Ok(None);
        };
        let agent_prompt = agent_prompt.trim();
        if agent_prompt.is_empty() {
            self.push_timeline(TimelineRole::Notice, "usage: /agent-name <prompt>");
            self.last_notice = Some("usage: /agent-name <prompt>".to_owned());
            return Ok(None);
        }
        Ok(Some(self.start_agent_profile_invocation(
            profile_id.to_owned(),
            agent_prompt.to_owned(),
            prompt.to_owned(),
        )))
    }

    fn start_agent_profile_invocation(
        &mut self,
        profile_id: String,
        agent_prompt: String,
        prompt: String,
    ) -> AppAction {
        self.clear_pending_plan_approval();
        self.composer.input.clear();
        self.composer.input_cursor = 0;
        self.composer.input_paste_spans.clear();
        self.reset_slash_selector();
        self.timeline_scroll_back = 0;
        self.push_timeline(
            TimelineRole::User,
            sigil_kernel::safe_persistence_text(&prompt),
        );
        self.push_event("input", format!("invoked agent {profile_id}"));
        self.active_pane = PaneFocus::Composer;
        self.push_event("focus", current_focus_label(self));
        self.runtime.is_busy = true;
        self.runtime.run_phase = RunPhase::Agent(profile_id.clone());
        self.last_notice = Some(format!("waiting for agent @{profile_id}"));
        self.runtime.last_phase_marker = None;
        self.push_phase_marker(format!("agent|{profile_id}"));
        self.streaming_assistant_index = None;
        self.streaming_reasoning_index = None;
        self.composer.mode = ComposerMode::Build;
        self.refresh_usage_sidebar_cache();
        AppAction::InvokeAgentProfile {
            profile_id,
            prompt: agent_prompt,
            parent_prompt: prompt,
        }
    }

    pub(in crate::app) fn execute_skill_slash_command(
        &mut self,
        command: &ResolvedSlashCommand,
        prompt: &str,
    ) -> Result<Option<AppAction>> {
        let Some(skill_id) = command.canonical.strip_prefix('/') else {
            return Ok(None);
        };
        let Some(skill) = self.exact_skill_descriptor(skill_id) else {
            return Ok(None);
        };
        let item_kind = slash_skill_display_kind(&skill);
        if self.runtime.is_busy {
            self.push_timeline(TimelineRole::Notice, format!("busy; use {item_kind} later"));
            self.last_notice = Some(format!("busy; use {item_kind} later"));
            return Ok(None);
        }
        if !skill.enabled {
            self.push_timeline(
                TimelineRole::Notice,
                format!("{item_kind} {skill_id} is disabled"),
            );
            self.last_notice = Some(format!("{item_kind} {skill_id} is disabled"));
            return Ok(None);
        }
        if skill.trust != SkillTrustState::Trusted {
            self.push_timeline(
                TimelineRole::Notice,
                format!("{item_kind} {skill_id} is not trusted"),
            );
            self.last_notice = Some(format!("{item_kind} {skill_id} is not trusted"));
            return Ok(None);
        }
        if !skill.user_invocable {
            self.push_timeline(
                TimelineRole::Notice,
                format!("{item_kind} {skill_id} is not user-invocable"),
            );
            self.last_notice = Some(format!("{item_kind} {skill_id} is not user-invocable"));
            return Ok(None);
        }

        self.timeline_scroll_back = 0;
        self.push_timeline(
            TimelineRole::User,
            sigil_kernel::safe_persistence_text(prompt),
        );
        self.push_event("input", format!("invoked {item_kind} {skill_id}"));
        self.active_pane = PaneFocus::Composer;
        self.push_event("focus", current_focus_label(self));
        self.runtime.is_busy = true;
        self.runtime.run_phase = RunPhase::Thinking;
        self.runtime.last_phase_marker = None;
        self.streaming_assistant_index = None;
        self.streaming_reasoning_index = None;
        self.refresh_usage_sidebar_cache();

        let arguments = command.arg.trim().to_owned();
        match skill.run_as {
            SkillRunMode::Inline => {
                self.last_notice = Some(format!("using {item_kind} {skill_id}"));
                self.push_phase_marker(format!("thinking|{}", self.runtime.model_name));
                Ok(Some(AppAction::InvokeInlineSkill {
                    skill_id: skill_id.to_owned(),
                    arguments,
                }))
            }
            SkillRunMode::ChildSession => {
                self.last_notice = Some(format!("invoking agent {skill_id}"));
                self.push_phase_marker(format!("task|{}", self.runtime.model_name));
                Ok(Some(AppAction::InvokeChildSessionSkill {
                    skill_id: skill_id.to_owned(),
                    arguments,
                }))
            }
        }
    }
}

fn slash_skill_display_kind(skill: &SkillDescriptor) -> &'static str {
    if matches!(skill.run_as, SkillRunMode::ChildSession) {
        "agent"
    } else if skill.entrypoint.starts_with(Path::new(".sigil/commands")) {
        "command"
    } else {
        "skill"
    }
}
