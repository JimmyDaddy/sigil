use super::*;
use serde_json::json;
use sigil_kernel::{
    AgentRunResult, ApprovalMode, PermissionRisk, RunEvent, ToolAccess, ToolCall, ToolCategory,
    ToolOperation, ToolPreviewCapability, ToolSpec,
};
use sigil_runtime::McpElicitationRequest;

fn enabled_config(method: TerminalNotificationMethod) -> TerminalNotificationConfig {
    TerminalNotificationConfig {
        enabled: true,
        method,
        minimum_run_duration_ms: 10_000,
    }
}

fn completed_run() -> WorkerMessage {
    WorkerMessage::RunFinished {
        result: agent_run_result(),
        entries: Vec::new(),
    }
}

fn agent_run_result() -> AgentRunResult {
    AgentRunResult {
        final_text: "private reply canary".to_owned(),
        tool_calls: 0,
        final_message_id: None,
    }
}

fn approval_message(call_id: &str) -> WorkerMessage {
    WorkerMessage::Event(Box::new(RunEvent::ToolApprovalRequested {
        call: ToolCall {
            id: call_id.to_owned(),
            name: "private-tool-canary".to_owned(),
            args_json: "{\"private\":true}".to_owned(),
        },
        spec: ToolSpec {
            name: "private-tool-canary".to_owned(),
            description: "private description canary".to_owned(),
            input_schema: json!({"type": "object"}),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            network_effect: None,
            preview: ToolPreviewCapability::Required,
        },
        subjects: Vec::new(),
        network_effect: None,
        local_policy_decision: ApprovalMode::Ask,
        network_policy_decision: ApprovalMode::Allow,
        source_policy_decision: ApprovalMode::Allow,
        operation: ToolOperation::OverwriteFile,
        risk: PermissionRisk::Medium,
        subject_zones: Vec::new(),
        confirmation: None,
        snapshot_required: false,
        command_permission_matches: Vec::new(),
        preview: None,
    }))
}

fn approval_resolved_message(call_id: &str) -> WorkerMessage {
    WorkerMessage::Event(Box::new(RunEvent::ToolApprovalResolved {
        call_id: call_id.to_owned(),
        approved: true,
        reason: None,
    }))
}

#[test]
fn long_run_notification_honors_default_off_threshold_and_focus() {
    let start = Instant::now();
    let environment = TerminalNotificationEnvironment::default();
    let mut controller =
        AttentionController::new(TerminalNotificationConfig::default(), environment.clone());

    controller.observe(
        &WorkerMessage::RunStarted {
            prompt: "private prompt canary".to_owned(),
        },
        start,
    );
    controller.observe(&completed_run(), start + Duration::from_secs(30));
    assert_eq!(
        controller
            .emit_pending(&mut Vec::new())
            .expect("disabled controller should emit no bytes"),
        0
    );

    controller.update_config(enabled_config(TerminalNotificationMethod::Bell));
    controller.observe(
        &WorkerMessage::RunStarted {
            prompt: "private prompt canary".to_owned(),
        },
        start + Duration::from_secs(40),
    );
    controller.observe(&completed_run(), start + Duration::from_secs(45));
    assert_eq!(
        controller
            .emit_pending(&mut Vec::new())
            .expect("short run should emit no bytes"),
        0
    );

    controller.observe(
        &WorkerMessage::RunStarted {
            prompt: "private prompt canary".to_owned(),
        },
        start + Duration::from_secs(50),
    );
    controller.observe_focus(true);
    controller.observe(&completed_run(), start + Duration::from_secs(65));
    assert_eq!(
        controller
            .emit_pending(&mut Vec::new())
            .expect("focused terminal should emit no bytes"),
        0
    );

    controller.observe_focus(false);
    controller.observe(
        &WorkerMessage::RunStarted {
            prompt: "private prompt canary".to_owned(),
        },
        start + Duration::from_secs(70),
    );
    controller.observe(&completed_run(), start + Duration::from_secs(85));
    let mut bytes = Vec::new();
    assert_eq!(
        controller
            .emit_pending(&mut bytes)
            .expect("eligible long run should emit"),
        1
    );
    assert_eq!(bytes, b"\x07");
}

#[test]
fn enabling_notifications_does_not_replay_or_time_disabled_activity() {
    let start = Instant::now();
    let environment = TerminalNotificationEnvironment::default();
    let mut controller =
        AttentionController::new(TerminalNotificationConfig::default(), environment);

    controller.observe(
        &WorkerMessage::RunStarted {
            prompt: "private prompt canary".to_owned(),
        },
        start,
    );
    controller.update_config(enabled_config(TerminalNotificationMethod::Bell));
    controller.observe(&completed_run(), start + Duration::from_secs(30));
    assert_eq!(
        controller
            .emit_pending(&mut Vec::new())
            .expect("enabling should not replay disabled activity"),
        0
    );

    controller.observe(
        &WorkerMessage::RunStarted {
            prompt: "new enabled run".to_owned(),
        },
        start + Duration::from_secs(31),
    );
    controller.observe(&completed_run(), start + Duration::from_secs(42));
    let mut bytes = Vec::new();
    assert_eq!(
        controller
            .emit_pending(&mut bytes)
            .expect("a run started after enabling should notify"),
        1
    );
    assert_eq!(bytes, b"\x07");
}

#[test]
fn run_failure_requires_an_active_run_and_uses_no_error_text() {
    let start = Instant::now();
    let mut controller = AttentionController::new(
        enabled_config(TerminalNotificationMethod::Osc9),
        TerminalNotificationEnvironment::default(),
    );
    controller.observe(
        &WorkerMessage::RunFailed("private error canary".to_owned()),
        start,
    );
    assert_eq!(
        controller
            .emit_pending(&mut Vec::new())
            .expect("unrelated failure should emit no bytes"),
        0
    );

    controller.observe(
        &WorkerMessage::RunStarted {
            prompt: "private prompt canary".to_owned(),
        },
        start + Duration::from_secs(1),
    );
    controller.observe(
        &WorkerMessage::RunFailed("private error canary".to_owned()),
        start + Duration::from_secs(2),
    );
    let mut bytes = Vec::new();
    assert_eq!(
        controller
            .emit_pending(&mut bytes)
            .expect("active run failure should emit"),
        1
    );
    let rendered = String::from_utf8(bytes).expect("OSC sequence should be UTF-8");
    assert_eq!(
        rendered,
        "\x1b]9;Sigil run failed | Open Sigil for details.\x1b\\"
    );
    assert!(!rendered.contains("private"));
}

#[test]
fn attention_cooldown_is_scoped_to_request_identity_and_resolution() {
    let start = Instant::now();
    let mut controller = AttentionController::new(
        enabled_config(TerminalNotificationMethod::Bell),
        TerminalNotificationEnvironment::default(),
    );

    controller.observe(&approval_message("call-1"), start);
    controller.observe(&approval_message("call-1"), start + Duration::from_secs(19));
    controller.observe(&approval_message("call-2"), start + Duration::from_secs(19));
    controller.observe(&approval_message("call-1"), start + Duration::from_secs(20));
    controller.observe(
        &approval_resolved_message("call-1"),
        start + Duration::from_secs(21),
    );
    controller.observe(&approval_message("call-1"), start + Duration::from_secs(21));

    let mut bytes = Vec::new();
    assert_eq!(
        controller
            .emit_pending(&mut bytes)
            .expect("cooldown test notifications should emit"),
        4
    );
    assert_eq!(bytes, b"\x07\x07\x07\x07");
}

#[test]
fn foreground_run_identities_do_not_overwrite_or_suppress_each_other() {
    let start = Instant::now();
    let mut controller = AttentionController::new(
        enabled_config(TerminalNotificationMethod::Bell),
        TerminalNotificationEnvironment::default(),
    );

    controller.observe(
        &WorkerMessage::RunStarted {
            prompt: "main".to_owned(),
        },
        start,
    );
    controller.observe(
        &WorkerMessage::PlanRunStarted {
            prompt: "plan".to_owned(),
        },
        start,
    );
    controller.observe(
        &WorkerMessage::AgentRunStarted {
            profile_id: "review".to_owned(),
            prompt: "agent".to_owned(),
        },
        start,
    );
    controller.observe(
        &WorkerMessage::TaskRunStarted {
            task_id: "task-1".to_owned(),
            objective: "task".to_owned(),
        },
        start,
    );

    let finished_at = start + Duration::from_secs(11);
    controller.observe(&completed_run(), finished_at);
    controller.observe(
        &WorkerMessage::PlanRunFinished {
            result: agent_run_result(),
            entries: Vec::new(),
        },
        finished_at,
    );
    controller.observe(
        &WorkerMessage::AgentRunFinished {
            profile_id: "review".to_owned(),
            result: agent_run_result(),
            entries: Vec::new(),
        },
        finished_at,
    );
    controller.observe(
        &WorkerMessage::TaskRunFinished {
            task_id: "task-1".to_owned(),
            status: TaskRunStatus::Completed,
            entries: Vec::new(),
        },
        finished_at,
    );

    controller.observe(
        &WorkerMessage::SkillRunStarted {
            skill_id: "skill-1".to_owned(),
            prompt: "skill".to_owned(),
        },
        start + Duration::from_secs(12),
    );
    controller.observe(&completed_run(), start + Duration::from_secs(23));

    controller.observe(
        &WorkerMessage::RunStarted {
            prompt: "follow-up".to_owned(),
        },
        start + Duration::from_secs(24),
    );
    controller.observe(&completed_run(), start + Duration::from_secs(35));

    let mut bytes = Vec::new();
    assert_eq!(
        controller
            .emit_pending(&mut bytes)
            .expect("each foreground run identity should emit"),
        6
    );
    assert_eq!(bytes, b"\x07\x07\x07\x07\x07\x07");
}

#[test]
fn worker_attention_events_map_to_fixed_approval_and_input_signals() {
    let start = Instant::now();
    let mut controller = AttentionController::new(
        enabled_config(TerminalNotificationMethod::Bell),
        TerminalNotificationEnvironment::default(),
    );
    let approval = approval_message("private-call-canary");
    controller.observe(&approval, start);

    let (response_tx, _response_rx) = tokio::sync::oneshot::channel();
    let elicitation = WorkerMessage::McpElicitationRequest {
        request: McpElicitationRequest {
            server_name: "private-server-canary".to_owned(),
            message: "private MCP message canary".to_owned(),
            requested_schema: json!({"type": "object"}),
        },
        response_tx,
    };
    controller.observe(&elicitation, start + Duration::from_secs(1));

    let mut bytes = Vec::new();
    assert_eq!(
        controller
            .emit_pending(&mut bytes)
            .expect("approval and elicitation signals should emit"),
        2
    );
    assert_eq!(bytes, b"\x07\x07");
}

#[test]
fn codecs_use_st_and_multiplexer_passthrough_without_dynamic_material() {
    let plain = TerminalNotificationEnvironment::default();
    assert_eq!(
        encode_notification(
            AttentionSignal::ApprovalRequired,
            TerminalNotificationMethod::Osc777,
            &plain,
        ),
        b"\x1b]777;notify;Sigil needs your attention;Tool approval required.\x1b\\"
    );

    let tmux = TerminalNotificationEnvironment {
        tmux: true,
        ..TerminalNotificationEnvironment::default()
    };
    let tmux_bytes = encode_notification(
        AttentionSignal::InputRequired,
        TerminalNotificationMethod::Osc9,
        &tmux,
    );
    assert!(tmux_bytes.starts_with(b"\x1bPtmux;\x1b\x1b]9;"));
    assert!(tmux_bytes.ends_with(b"\x1b\x1b\\\x1b\\"));

    let screen = TerminalNotificationEnvironment {
        screen: true,
        ..TerminalNotificationEnvironment::default()
    };
    let screen_bytes = encode_notification(
        AttentionSignal::LongRunComplete,
        TerminalNotificationMethod::Osc9,
        &screen,
    );
    assert!(screen_bytes.starts_with(b"\x1bP\x1b]9;"));
    assert!(screen_bytes.ends_with(b"\x1b\\\x1b\\"));
}

#[test]
fn auto_method_prefers_known_protocols_and_falls_back_to_bell() {
    let iterm = TerminalNotificationEnvironment {
        term_program: Some("iTerm.app".to_owned()),
        ..TerminalNotificationEnvironment::default()
    };
    assert_eq!(
        iterm.resolve_method(TerminalNotificationMethod::Auto),
        TerminalNotificationMethod::Osc9
    );

    let ghostty = TerminalNotificationEnvironment {
        term_program: Some("ghostty".to_owned()),
        ..TerminalNotificationEnvironment::default()
    };
    assert_eq!(
        ghostty.resolve_method(TerminalNotificationMethod::Auto),
        TerminalNotificationMethod::Osc9
    );

    let vte = TerminalNotificationEnvironment {
        has_vte: true,
        ..TerminalNotificationEnvironment::default()
    };
    assert_eq!(
        vte.resolve_method(TerminalNotificationMethod::Auto),
        TerminalNotificationMethod::Osc777
    );

    assert_eq!(
        TerminalNotificationEnvironment::default().resolve_method(TerminalNotificationMethod::Auto),
        TerminalNotificationMethod::Bell
    );
}

#[test]
fn terminal_write_failure_is_nonfatal_and_does_not_retain_payload() {
    struct FailingWriter;

    impl std::io::Write for FailingWriter {
        fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("injected terminal write failure"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let start = Instant::now();
    let mut controller = AttentionController::new(
        enabled_config(TerminalNotificationMethod::Osc9),
        TerminalNotificationEnvironment::default(),
    );
    controller.observe(
        &WorkerMessage::RunStarted {
            prompt: "private prompt canary".to_owned(),
        },
        start,
    );
    controller.observe(
        &WorkerMessage::RunFailed("private error canary".to_owned()),
        start + Duration::from_secs(1),
    );

    assert_eq!(controller.emit_pending_nonfatal(&mut FailingWriter), 0);
    assert_eq!(
        controller
            .emit_pending(&mut Vec::new())
            .expect("failed terminal write should leave no retained payload"),
        0
    );
}
