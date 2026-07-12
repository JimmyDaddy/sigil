use std::path::PathBuf;

use ratatui::{Terminal, backend::TestBackend};
use sigil_kernel::{
    EgressDataCategory, EgressDisclosureKind, EgressNetworkRoute, EventHandler,
    PreEgressDisclosure, RunEvent, ToolResult, ToolResultMeta,
};

use crate::{
    app::AppState,
    runner::{McpActivationStatus, WorkerMessage},
};

use super::{
    EGRESS_DISCLOSURE_HEIGHT, egress_disclosure_layout, render_active_egress_disclosure_card,
};

fn disclosure(correlation_id: &str) -> PreEgressDisclosure {
    PreEgressDisclosure::new(
        EgressDisclosureKind::Query,
        Some(correlation_id.to_owned()),
        "exa-anonymous-2026-06-29",
        "tui",
        "Exa no-key free tier",
        "route-fingerprint",
        "profile-fingerprint",
        "https://mcp.exa.ai/",
        "https://mcp.exa.ai/",
        EgressNetworkRoute::Direct,
        vec![EgressDataCategory::SearchQuery],
    )
    .expect("valid safe disclosure")
}

fn transport_disclosure(id: &str) -> PreEgressDisclosure {
    PreEgressDisclosure::new(
        EgressDisclosureKind::Transport,
        None,
        id,
        "tui",
        "Anonymous Exa MCP transport",
        format!("route-fingerprint-{id}"),
        "profile-fingerprint",
        "https://mcp.exa.ai/",
        "http://127.0.0.1:7897/",
        EgressNetworkRoute::ProxyRemote,
        vec![EgressDataCategory::ConnectionMetadata],
    )
    .expect("valid transport disclosure")
}

fn render_disclosure_frame(app: &AppState, terminal: &mut Terminal<TestBackend>) -> bool {
    app.begin_egress_disclosure_frame();
    let mut rendered = false;
    terminal
        .draw(|frame| {
            let theme = crate::ui::theme::Theme::default();
            rendered = render_active_egress_disclosure_card(frame, frame.area(), app, &theme);
        })
        .expect("frame should render");
    rendered
}

#[test]
fn active_disclosure_reserves_a_non_overlapping_top_strip() {
    let mut app = AppState::from_setup(PathBuf::from("sigil.toml"), PathBuf::from("."), None);
    let (receipt_tx, _receipt_rx) = tokio::sync::oneshot::channel();
    app.handle_worker_message(WorkerMessage::EgressDisclosureRequested {
        disclosure: disclosure("query-layout"),
        receipt_tx,
    })
    .expect("disclosure request");

    let full = ratatui::layout::Rect::new(0, 0, 80, 24);
    let (strip, content) = egress_disclosure_layout(full, &app);
    let strip = strip.expect("active disclosure strip");
    assert_eq!(strip, ratatui::layout::Rect::new(0, 0, 80, 5));
    assert_eq!(strip.height, EGRESS_DISCLOSURE_HEIGHT);
    assert_eq!(content, ratatui::layout::Rect::new(0, 5, 80, 19));
    assert_eq!(strip.y.saturating_add(strip.height), content.y);
}

#[test]
fn card_renders_before_the_tui_acks_the_matching_receipt() {
    let mut app = AppState::from_setup(PathBuf::from("sigil.toml"), PathBuf::from("."), None);
    let (receipt_tx, mut receipt_rx) = tokio::sync::oneshot::channel();
    app.handle_worker_message(WorkerMessage::EgressDisclosureRequested {
        disclosure: disclosure("query-1"),
        receipt_tx,
    })
    .expect("app should accept the disclosure request");

    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
    let rendered = render_disclosure_frame(&app, &mut terminal);

    assert!(rendered);
    assert!(matches!(
        receipt_rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    assert!(app.acknowledge_active_egress_disclosure_frame());
    let receipt = futures::executor::block_on(receipt_rx)
        .expect("receipt should arrive after frame acknowledgement")
        .expect("frame acknowledgement should be valid");
    assert_eq!(receipt.correlation_id(), Some("query-1"));
    assert_eq!(receipt.sink_fingerprint(), "tui-active-card-frame-v1");
    let visible = app
        .active_egress_disclosure_card()
        .expect("acknowledged disclosure should remain visible until the operation finishes");
    assert_eq!(visible.disclosure_count, 1);
    assert!(render_disclosure_frame(&app, &mut terminal));

    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-search",
        "websearch",
        "result",
        ToolResultMeta::default(),
    )))
    .expect("tool result should clear the completed disclosure operation");
    assert!(app.active_egress_disclosure_card().is_none());
}

#[test]
fn two_queries_require_two_distinct_frame_acknowledgements() {
    let mut app = AppState::from_setup(PathBuf::from("sigil.toml"), PathBuf::from("."), None);
    let (first_tx, first_rx) = tokio::sync::oneshot::channel();
    let (second_tx, mut second_rx) = tokio::sync::oneshot::channel();
    app.handle_worker_message(WorkerMessage::EgressDisclosureRequested {
        disclosure: disclosure("query-1"),
        receipt_tx: first_tx,
    })
    .expect("first request");
    app.handle_worker_message(WorkerMessage::EgressDisclosureRequested {
        disclosure: disclosure("query-2"),
        receipt_tx: second_tx,
    })
    .expect("second request");

    assert!(!app.acknowledge_active_egress_disclosure_frame());
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
    assert!(render_disclosure_frame(&app, &mut terminal));
    assert!(app.acknowledge_active_egress_disclosure_frame());
    let first = futures::executor::block_on(first_rx)
        .expect("first receipt")
        .expect("first success");
    assert_eq!(first.correlation_id(), Some("query-1"));
    assert!(matches!(
        second_rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    assert!(render_disclosure_frame(&app, &mut terminal));
    assert!(app.acknowledge_active_egress_disclosure_frame());
    let second = futures::executor::block_on(second_rx)
        .expect("second receipt")
        .expect("second success");
    assert_eq!(second.correlation_id(), Some("query-2"));
    let visible = app
        .active_egress_disclosure_card()
        .expect("aggregated disclosure should remain visible");
    assert_eq!(visible.disclosure_count, 2);
}

#[test]
fn transport_and_query_disclosures_merge_into_one_continuous_operation_card() {
    let mut app = AppState::from_setup(PathBuf::from("sigil.toml"), PathBuf::from("."), None);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");

    for disclosure in [transport_disclosure("transport-1"), disclosure("query-1")] {
        let (receipt_tx, receipt_rx) = tokio::sync::oneshot::channel();
        app.handle_worker_message(WorkerMessage::EgressDisclosureRequested {
            disclosure,
            receipt_tx,
        })
        .expect("disclosure request");
        assert!(render_disclosure_frame(&app, &mut terminal));
        assert!(app.acknowledge_active_egress_disclosure_frame());
        futures::executor::block_on(receipt_rx)
            .expect("receipt")
            .expect("successful frame receipt");
    }

    let visible = app
        .active_egress_disclosure_card()
        .expect("operation card should remain visible");
    assert_eq!(visible.disclosure_count, 2);
    assert!(visible.title.contains("query disclosure"));
    assert!(visible.route.contains("environment proxy"));
    assert!(visible.route.contains("direct"));
    assert!(visible.data_categories.contains("connection metadata"));
    assert!(visible.data_categories.contains("search query"));

    app.handle_worker_message(WorkerMessage::McpActivationStatus {
        server_name: Some("bundled-exa".to_owned()),
        status: McpActivationStatus::Ready {
            added_tools: 1,
            process_coverage: None,
        },
    })
    .expect("terminal activation status");
    assert!(app.active_egress_disclosure_card().is_none());
}
