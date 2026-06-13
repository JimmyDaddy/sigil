use std::{
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Result, anyhow};
use serde_json::json;
use sigil_kernel::{ControlEntry, McpElicitationDecision};
use sigil_runtime::{
    McpElicitationAction, McpElicitationHandler, McpElicitationRequest, McpElicitationResponse,
};

use super::super::{WorkerMessage, elicitation_bridge::ChannelMcpElicitationHandler};

#[test]
fn elicitation_handler_supports_requests_and_round_trips_response() -> Result<()> {
    let (message_tx, message_rx) = mpsc::channel();
    let handler = ChannelMcpElicitationHandler::new(message_tx);
    assert!(handler.supports_elicitation());

    let request = McpElicitationRequest {
        server_name: "docs".to_owned(),
        message: "Need confirmation".to_owned(),
        requested_schema: json!({
            "type": "object",
            "properties": {
                "answer": { "type": "string" }
            }
        }),
    };
    let expected_request = request.clone();
    let expected_response = McpElicitationResponse::accept(json!({ "answer": "yes" }));
    let thread_response = expected_response.clone();

    let responder = thread::spawn(move || -> Result<()> {
        let message = message_rx
            .recv_timeout(Duration::from_secs(1))
            .map_err(|error| anyhow!("timed out waiting for elicitation request: {error}"))?;
        let WorkerMessage::McpElicitationRequest {
            request,
            response_tx,
        } = message
        else {
            return Err(anyhow!("expected MCP elicitation request"));
        };
        assert_eq!(request, expected_request);
        response_tx
            .send(thread_response)
            .map_err(|_| anyhow!("failed to send elicitation response"))?;
        Ok(())
    });

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let response = runtime.block_on(handler.elicit(request))?;
    assert_eq!(response, expected_response);

    responder
        .join()
        .map_err(|_| anyhow!("elicitation responder thread panicked"))??;
    Ok(())
}

#[test]
fn elicitation_handler_records_decline_and_cancel_audit_decisions() -> Result<()> {
    for (response, expected) in [
        (
            McpElicitationResponse {
                action: McpElicitationAction::Decline,
                content: None,
            },
            McpElicitationDecision::Declined,
        ),
        (
            McpElicitationResponse {
                action: McpElicitationAction::Cancel,
                content: None,
            },
            McpElicitationDecision::Cancelled,
        ),
    ] {
        let control = run_elicitation_with_audit(response)?;
        assert!(matches!(
            control,
            ControlEntry::McpElicitation(entry) if entry.action == expected
        ));
    }
    Ok(())
}

#[test]
fn elicitation_handler_records_redacted_accept_audit_entry() -> Result<()> {
    let request = McpElicitationRequest {
        server_name: "filesystem".to_owned(),
        message: "Need workspace path".to_owned(),
        requested_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "title": "Path" },
                "token": { "type": "string", "title": "Token" }
            },
            "required": ["path"]
        }),
    };
    let control = run_elicitation_with_audit_request(
        request,
        McpElicitationResponse::accept(json!({
            "path": "src/lib.rs",
            "token": "secret-token-value"
        })),
    )?;
    let serialized = serde_json::to_string(&control)?;

    assert!(!serialized.contains("src/lib.rs"));
    assert!(!serialized.contains("secret-token-value"));
    assert!(matches!(
        control,
        ControlEntry::McpElicitation(entry)
            if entry.server_name == "filesystem"
                && entry.action == McpElicitationDecision::Accepted
                && entry.content_redacted
                && entry.content_field_names == vec!["path".to_owned(), "token".to_owned()]
    ));
    Ok(())
}

#[test]
fn elicitation_handler_errors_when_tui_channel_is_closed() -> Result<()> {
    let (message_tx, message_rx) = mpsc::channel();
    drop(message_rx);
    let handler = ChannelMcpElicitationHandler::new(message_tx);
    let request = McpElicitationRequest {
        server_name: "docs".to_owned(),
        message: "Need confirmation".to_owned(),
        requested_schema: json!({ "type": "object" }),
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let error = runtime
        .block_on(handler.elicit(request))
        .expect_err("closed TUI channel should fail");

    assert!(
        error
            .to_string()
            .contains("failed to send MCP elicitation request to TUI")
    );
    Ok(())
}

fn run_elicitation_with_audit(response: McpElicitationResponse) -> Result<ControlEntry> {
    run_elicitation_with_audit_request(
        McpElicitationRequest {
            server_name: "docs".to_owned(),
            message: "Need confirmation".to_owned(),
            requested_schema: json!({ "type": "object" }),
        },
        response,
    )
}

fn run_elicitation_with_audit_request(
    request: McpElicitationRequest,
    response: McpElicitationResponse,
) -> Result<ControlEntry> {
    let (message_tx, message_rx) = mpsc::channel();
    let audit_buffer = Arc::new(Mutex::new(Vec::new()));
    let handler = ChannelMcpElicitationHandler::new(message_tx);
    handler.set_audit_buffer(Some(Arc::clone(&audit_buffer)));

    let responder = thread::spawn(move || -> Result<()> {
        let WorkerMessage::McpElicitationRequest { response_tx, .. } = message_rx
            .recv_timeout(Duration::from_secs(1))
            .map_err(|error| anyhow!("timed out waiting for elicitation request: {error}"))?
        else {
            return Err(anyhow!("expected MCP elicitation request"));
        };
        response_tx
            .send(response)
            .map_err(|_| anyhow!("failed to send elicitation response"))?;
        Ok(())
    });

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(handler.elicit(request))?;
    responder
        .join()
        .map_err(|_| anyhow!("elicitation responder thread panicked"))??;
    audit_buffer
        .lock()
        .map_err(|_| anyhow!("audit buffer lock poisoned"))?
        .pop()
        .ok_or_else(|| anyhow!("expected audit entry"))
}

#[test]
fn elicitation_handler_errors_when_response_channel_is_closed() -> Result<()> {
    let (message_tx, message_rx) = mpsc::channel();
    let handler = ChannelMcpElicitationHandler::new(message_tx);
    let request = McpElicitationRequest {
        server_name: "docs".to_owned(),
        message: "Need confirmation".to_owned(),
        requested_schema: json!({ "type": "object" }),
    };

    let responder = thread::spawn(move || -> Result<()> {
        let message = message_rx
            .recv_timeout(Duration::from_secs(1))
            .map_err(|error| anyhow!("timed out waiting for elicitation request: {error}"))?;
        let WorkerMessage::McpElicitationRequest { response_tx, .. } = message else {
            return Err(anyhow!("expected MCP elicitation request"));
        };
        drop(response_tx);
        Ok(())
    });

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let error = runtime
        .block_on(handler.elicit(request))
        .expect_err("closed response channel should fail");

    assert!(
        error
            .to_string()
            .contains("MCP elicitation response channel closed")
    );

    responder
        .join()
        .map_err(|_| anyhow!("elicitation responder thread panicked"))??;
    Ok(())
}
