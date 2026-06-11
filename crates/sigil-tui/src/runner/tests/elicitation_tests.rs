use std::{sync::mpsc, thread, time::Duration};

use anyhow::{Result, anyhow};
use serde_json::json;
use sigil_runtime::{McpElicitationHandler, McpElicitationRequest, McpElicitationResponse};

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
