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
        result: AgentRunResult {
            final_text: "private reply canary".to_owned(),
            tool_calls: 0,
            final_message_id: None,
        },
        entries: Vec::new(),
    }
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
fn attention_cooldown_is_per_fixed_signal() {
    let start = Instant::now();
    let mut controller = AttentionController::new(
        enabled_config(TerminalNotificationMethod::Bell),
        TerminalNotificationEnvironment::default(),
    );

    controller.queue_signal(AttentionSignal::ApprovalRequired, start);
    controller.queue_signal(
        AttentionSignal::ApprovalRequired,
        start + Duration::from_secs(19),
    );
    controller.queue_signal(
        AttentionSignal::InputRequired,
        start + Duration::from_secs(19),
    );
    controller.queue_signal(
        AttentionSignal::ApprovalRequired,
        start + Duration::from_secs(20),
    );

    let mut bytes = Vec::new();
    assert_eq!(
        controller
            .emit_pending(&mut bytes)
            .expect("cooldown test notifications should emit"),
        3
    );
    assert_eq!(bytes, b"\x07\x07\x07");
}

#[test]
fn worker_attention_events_map_to_fixed_approval_and_input_signals() {
    let start = Instant::now();
    let mut controller = AttentionController::new(
        enabled_config(TerminalNotificationMethod::Bell),
        TerminalNotificationEnvironment::default(),
    );
    let approval = WorkerMessage::Event(Box::new(RunEvent::ToolApprovalRequested {
        call: ToolCall {
            id: "private-call-canary".to_owned(),
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
    }));
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
