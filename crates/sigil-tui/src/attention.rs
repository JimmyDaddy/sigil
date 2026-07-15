use std::{
    collections::HashMap,
    io,
    time::{Duration, Instant},
};

#[cfg(not(test))]
use std::env;

use sha2::{Digest, Sha256};
use sigil_kernel::{
    RunEvent, TaskRunStatus, TerminalNotificationConfig, TerminalNotificationMethod,
};

use crate::runner::WorkerMessage;

const ATTENTION_COOLDOWN: Duration = Duration::from_secs(20);
const OSC_ST: &str = "\x1b\\";

/// Fixed, privacy-safe attention categories. Dynamic event fields are intentionally absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttentionSignal {
    LongRunComplete,
    ApprovalRequired,
    RunFailed,
    InputRequired,
}

impl AttentionSignal {
    const fn title(self) -> &'static str {
        match self {
            Self::LongRunComplete => "Sigil session complete",
            Self::ApprovalRequired | Self::InputRequired => "Sigil needs your attention",
            Self::RunFailed => "Sigil run failed",
        }
    }

    const fn body(self) -> &'static str {
        match self {
            Self::LongRunComplete => "Long run finished.",
            Self::ApprovalRequired => "Tool approval required.",
            Self::RunFailed => "Open Sigil for details.",
            Self::InputRequired => "Input required to continue.",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TerminalNotificationEnvironment {
    term_program: Option<String>,
    term: Option<String>,
    has_vte: bool,
    has_wezterm: bool,
    has_kitty: bool,
    tmux: bool,
    screen: bool,
}

impl TerminalNotificationEnvironment {
    #[cfg(not(test))]
    fn from_current_process() -> Self {
        Self {
            term_program: non_empty_env("TERM_PROGRAM"),
            term: non_empty_env("TERM"),
            has_vte: non_empty_env("VTE_VERSION").is_some(),
            has_wezterm: non_empty_env("WEZTERM_PANE").is_some(),
            has_kitty: non_empty_env("KITTY_WINDOW_ID").is_some(),
            tmux: non_empty_env("TMUX").is_some(),
            screen: non_empty_env("STY").is_some(),
        }
    }

    fn resolve_method(&self, configured: TerminalNotificationMethod) -> TerminalNotificationMethod {
        if configured != TerminalNotificationMethod::Auto {
            return configured;
        }

        let term_program = self.term_program.as_deref().unwrap_or_default();
        let term = self.term.as_deref().unwrap_or_default();
        if matches!(term_program, "iTerm.app" | "WezTerm")
            || term_program.eq_ignore_ascii_case("ghostty")
            || self.has_wezterm
            || self.has_kitty
            || term.contains("kitty")
            || term.contains("ghostty")
        {
            TerminalNotificationMethod::Osc9
        } else if self.has_vte || term.contains("rxvt") {
            TerminalNotificationMethod::Osc777
        } else {
            TerminalNotificationMethod::Bell
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ForegroundRunKind {
    Main,
    Skill(String),
    Plan,
    Agent(String),
    Task(String),
}

#[derive(Debug, Clone, Copy)]
struct ActiveForegroundRun {
    id: u64,
    started_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum AttentionKey {
    RunCompleted(u64),
    RunFailed(u64),
    Approval([u8; 32]),
    Input([u8; 32]),
}

#[cfg(not(test))]
fn non_empty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub(crate) struct AttentionController {
    config: TerminalNotificationConfig,
    environment: TerminalNotificationEnvironment,
    focused: Option<bool>,
    active_runs: HashMap<ForegroundRunKind, ActiveForegroundRun>,
    active_chat_kind: Option<ForegroundRunKind>,
    next_run_id: u64,
    last_emitted: HashMap<AttentionKey, Instant>,
    pending: Vec<AttentionSignal>,
}

impl AttentionController {
    #[cfg(not(test))]
    pub(crate) fn from_current_process(config: TerminalNotificationConfig) -> Self {
        Self::new(
            config,
            TerminalNotificationEnvironment::from_current_process(),
        )
    }

    pub(crate) fn new(
        config: TerminalNotificationConfig,
        environment: TerminalNotificationEnvironment,
    ) -> Self {
        Self {
            config,
            environment,
            focused: None,
            active_runs: HashMap::new(),
            active_chat_kind: None,
            next_run_id: 0,
            last_emitted: HashMap::new(),
            pending: Vec::new(),
        }
    }

    pub(crate) fn update_config(&mut self, config: TerminalNotificationConfig) {
        if self.config.enabled != config.enabled {
            self.focused = None;
            self.last_emitted.clear();
            self.pending.clear();
            self.clear_active_runs();
        }
        self.config = config;
    }

    #[cfg(not(test))]
    pub(crate) fn reset_focus_reliability(&mut self) {
        self.focused = None;
    }

    pub(crate) fn observe_focus(&mut self, focused: bool) {
        self.focused = Some(focused);
    }

    pub(crate) fn observe(&mut self, message: &WorkerMessage, now: Instant) {
        if !self.config.enabled {
            return;
        }
        match message {
            WorkerMessage::RunStarted { .. } => {
                self.start_chat_run(ForegroundRunKind::Main, now);
            }
            WorkerMessage::SkillRunStarted { skill_id, .. } => {
                self.start_chat_run(ForegroundRunKind::Skill(skill_id.clone()), now);
            }
            WorkerMessage::PlanRunStarted { .. } => {
                self.start_run(ForegroundRunKind::Plan, now);
            }
            WorkerMessage::AgentRunStarted { profile_id, .. } => {
                self.start_run(ForegroundRunKind::Agent(profile_id.clone()), now);
            }
            WorkerMessage::TaskRunStarted { task_id, .. } => {
                self.start_run(ForegroundRunKind::Task(task_id.clone()), now);
            }
            WorkerMessage::RunFinished { .. } => {
                self.finish_chat_run(now, AttentionSignal::LongRunComplete);
            }
            WorkerMessage::PlanRunFinished { .. } => {
                self.finish_run(
                    &ForegroundRunKind::Plan,
                    now,
                    AttentionSignal::LongRunComplete,
                );
            }
            WorkerMessage::AgentRunFinished { profile_id, .. } => {
                self.finish_run(
                    &ForegroundRunKind::Agent(profile_id.clone()),
                    now,
                    AttentionSignal::LongRunComplete,
                );
            }
            WorkerMessage::TaskRunFinished {
                task_id, status, ..
            } => match status {
                TaskRunStatus::Completed => {
                    self.finish_run(
                        &ForegroundRunKind::Task(task_id.clone()),
                        now,
                        AttentionSignal::LongRunComplete,
                    );
                }
                TaskRunStatus::Failed | TaskRunStatus::Interrupted => {
                    self.finish_run(
                        &ForegroundRunKind::Task(task_id.clone()),
                        now,
                        AttentionSignal::RunFailed,
                    );
                }
                TaskRunStatus::Cancelled => {
                    self.active_runs
                        .remove(&ForegroundRunKind::Task(task_id.clone()));
                }
                TaskRunStatus::Started | TaskRunStatus::Running | TaskRunStatus::Paused => {}
            },
            WorkerMessage::RunFailed(_) | WorkerMessage::RunInterrupted { .. } => {
                self.finish_unknown_run(now, AttentionSignal::RunFailed);
            }
            WorkerMessage::RunCancelled { .. } => {
                self.clear_active_runs();
            }
            WorkerMessage::Event(event) | WorkerMessage::AgentThreadEvent { event, .. } => {
                match event.as_ref() {
                    RunEvent::ToolApprovalRequested { call, .. } => {
                        self.queue_signal(
                            AttentionSignal::ApprovalRequired,
                            AttentionKey::Approval(identity_hash(&[call.id.as_bytes()])),
                            now,
                        );
                    }
                    RunEvent::ToolApprovalResolved { call_id, .. } => {
                        self.last_emitted
                            .remove(&AttentionKey::Approval(identity_hash(
                                &[call_id.as_bytes()],
                            )));
                    }
                    _ => {}
                }
            }
            WorkerMessage::McpElicitationRequest { request, .. } => {
                let schema = serde_json::to_vec(&request.requested_schema)
                    .expect("serializing a serde_json::Value cannot fail");
                self.queue_signal(
                    AttentionSignal::InputRequired,
                    AttentionKey::Input(identity_hash(&[
                        request.server_name.as_bytes(),
                        request.message.as_bytes(),
                        &schema,
                    ])),
                    now,
                );
            }
            _ => {}
        }
    }

    fn start_chat_run(&mut self, kind: ForegroundRunKind, now: Instant) {
        self.start_run(kind.clone(), now);
        self.active_chat_kind = Some(kind);
    }

    fn start_run(&mut self, kind: ForegroundRunKind, now: Instant) {
        self.next_run_id = self.next_run_id.wrapping_add(1);
        self.active_runs.insert(
            kind,
            ActiveForegroundRun {
                id: self.next_run_id,
                started_at: now,
            },
        );
    }

    fn finish_chat_run(&mut self, now: Instant, signal: AttentionSignal) {
        let Some(kind) = self.active_chat_kind.take() else {
            return;
        };
        self.finish_run(&kind, now, signal);
    }

    fn finish_run(&mut self, kind: &ForegroundRunKind, now: Instant, signal: AttentionSignal) {
        let Some(active_run) = self.active_runs.remove(kind) else {
            return;
        };
        if signal != AttentionSignal::LongRunComplete
            || now.saturating_duration_since(active_run.started_at)
                >= Duration::from_millis(self.config.minimum_run_duration_ms)
        {
            let key = match signal {
                AttentionSignal::LongRunComplete => AttentionKey::RunCompleted(active_run.id),
                AttentionSignal::RunFailed => AttentionKey::RunFailed(active_run.id),
                AttentionSignal::ApprovalRequired | AttentionSignal::InputRequired => {
                    unreachable!("run completion only accepts terminal run signals")
                }
            };
            self.queue_signal(signal, key, now);
        }
    }

    fn finish_unknown_run(&mut self, now: Instant, signal: AttentionSignal) {
        let active_run = self
            .active_chat_kind
            .take()
            .and_then(|kind| self.active_runs.remove(&kind))
            .or_else(|| {
                let kind = self
                    .active_runs
                    .iter()
                    .min_by_key(|(_, run)| run.id)
                    .map(|(kind, _)| kind.clone())?;
                self.active_runs.remove(&kind)
            });
        self.clear_active_runs();

        let Some(active_run) = active_run else {
            return;
        };
        let key = match signal {
            AttentionSignal::RunFailed => AttentionKey::RunFailed(active_run.id),
            AttentionSignal::LongRunComplete => AttentionKey::RunCompleted(active_run.id),
            AttentionSignal::ApprovalRequired | AttentionSignal::InputRequired => {
                unreachable!("unknown run finish only accepts terminal run signals")
            }
        };
        self.queue_signal(signal, key, now);
    }

    fn clear_active_runs(&mut self) {
        self.active_runs.clear();
        self.active_chat_kind = None;
    }

    fn queue_signal(&mut self, signal: AttentionSignal, key: AttentionKey, now: Instant) {
        if !self.config.enabled || self.focused == Some(true) {
            return;
        }
        self.last_emitted.retain(|_, emitted_at| {
            now.saturating_duration_since(*emitted_at) < ATTENTION_COOLDOWN
        });
        if self
            .last_emitted
            .get(&key)
            .is_some_and(|last| now.saturating_duration_since(*last) < ATTENTION_COOLDOWN)
        {
            return;
        }
        self.last_emitted.insert(key, now);
        self.pending.push(signal);
    }

    pub(crate) fn emit_pending<W: io::Write>(&mut self, writer: &mut W) -> io::Result<usize> {
        let pending = std::mem::take(&mut self.pending);
        for signal in pending.iter().copied() {
            let sequence = encode_notification(
                signal,
                self.environment.resolve_method(self.config.method),
                &self.environment,
            );
            writer.write_all(&sequence)?;
        }
        if !pending.is_empty() {
            writer.flush()?;
        }
        Ok(pending.len())
    }

    pub(crate) fn emit_pending_nonfatal<W: io::Write>(&mut self, writer: &mut W) -> usize {
        match self.emit_pending(writer) {
            Ok(emitted) => emitted,
            Err(error) => {
                tracing::debug!(%error, "failed to emit terminal attention notification");
                0
            }
        }
    }
}

fn identity_hash(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    hasher.finalize().into()
}

fn encode_notification(
    signal: AttentionSignal,
    method: TerminalNotificationMethod,
    environment: &TerminalNotificationEnvironment,
) -> Vec<u8> {
    if method == TerminalNotificationMethod::Bell {
        return vec![b'\x07'];
    }

    let sequence = match method {
        TerminalNotificationMethod::Osc9 => {
            format!("\x1b]9;{} | {}{OSC_ST}", signal.title(), signal.body())
        }
        TerminalNotificationMethod::Osc777 => format!(
            "\x1b]777;notify;{};{}{OSC_ST}",
            signal.title(),
            signal.body()
        ),
        TerminalNotificationMethod::Auto | TerminalNotificationMethod::Bell => unreachable!(
            "auto notification methods must be resolved and bell returns before OSC encoding"
        ),
    };

    wrap_with_passthrough(sequence, environment).into_bytes()
}

fn wrap_with_passthrough(
    sequence: String,
    environment: &TerminalNotificationEnvironment,
) -> String {
    if environment.tmux {
        format!("\x1bPtmux;{}\x1b\\", sequence.replace('\x1b', "\x1b\x1b"))
    } else if environment.screen {
        format!("\x1bP{sequence}\x1b\\")
    } else {
        sequence
    }
}

#[cfg(test)]
#[path = "tests/attention_tests.rs"]
mod tests;
