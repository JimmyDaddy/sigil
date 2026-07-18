use serde_json::{Value, json};

use crate::protocol::HTTP_PROTOCOL_VERSION;

/// OpenAPI version emitted for the MVP desktop/app-server command surface.
pub const HTTP_OPENAPI_VERSION: &str = "3.1.0";

/// Returns the MVP OpenAPI description for the local HTTP command surface.
///
/// The document intentionally covers only routes implemented by this crate.
#[must_use]
pub fn http_openapi_document() -> Value {
    json!({
        "openapi": HTTP_OPENAPI_VERSION,
        "info": {
            "title": "Sigil Local App Server API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Localhost-only adapter surface for desktop and future local clients."
        },
        "security": [{ "BearerAuth": [] }],
        "paths": {
            "/health": {
                "get": {
                    "summary": "Local listener health check",
                    "security": [],
                    "responses": {
                        "200": {
                            "description": "Listener is running",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/HealthResponse" }
                                }
                            }
                        }
                    }
                }
            },
            "/server-info": {
                "get": {
                    "summary": "Read immutable local server bootstrap metadata",
                    "responses": {
                        "200": {
                            "description": "Secret-free workspace/listener/protocol capabilities",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/ServerInfo" }
                                }
                            }
                        },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                }
            },
            "/openapi.json": {
                "get": {
                    "summary": "Read this authenticated local API description",
                    "responses": {
                        "200": { "description": "OpenAPI 3.1 document" },
                        "401": { "$ref": "#/components/responses/Unauthorized" }
                    }
                }
            },
            "/disclosures": {
                "get": {
                    "summary": "Replay safe durable egress disclosures",
                    "parameters": [{
                        "name": "Last-Event-ID",
                        "in": "header",
                        "required": false,
                        "schema": { "type": "string" }
                    }],
                    "responses": {
                        "200": {
                            "description": "Retained disclosure suffix",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/DisclosureListResponse" }
                                }
                            }
                        },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "409": { "$ref": "#/components/responses/Conflict" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                }
            },
            "/sessions": {
                "get": {
                    "summary": "List local session handles",
                    "responses": {
                        "200": {
                            "description": "Session list",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/SessionListResponse" }
                                }
                            }
                        },
                        "401": { "$ref": "#/components/responses/Unauthorized" }
                    }
                },
                "post": {
                    "summary": "Create a local session handle",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/SessionCreateRequest" }
                            }
                        }
                    },
                    "responses": {
                        "201": {
                            "description": "Session snapshot",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/SessionSnapshot" }
                                }
                            }
                        },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/sessions/open": {
                "post": {
                    "summary": "Reopen a durable workspace session as a local handle",
                    "description": "Revalidates the relative session reference and expected durable identity against current lifecycle and JSONL truth. SQLite catalog rows are candidates, not authorization.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/SessionOpenRequest" }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "New or existing idempotent local session snapshot",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/SessionSnapshot" }
                                }
                            }
                        },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "409": { "$ref": "#/components/responses/Conflict" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                }
            },
            "/session-catalog": {
                "get": {
                    "summary": "List durable historical sessions for the current workspace",
                    "description": "Reconciles the rebuildable SQLite projection from durable JSONL sources, then returns a generation-consistent keyset page. Active run, approval, and progress state are not included.",
                    "parameters": [
                        {
                            "name": "limit",
                            "in": "query",
                            "required": false,
                            "schema": { "type": "integer", "minimum": 1, "maximum": 100, "default": 50 }
                        },
                        {
                            "name": "cursor",
                            "in": "query",
                            "required": false,
                            "schema": { "type": "string" }
                        },
                        {
                            "name": "q",
                            "in": "query",
                            "required": false,
                            "description": "Literal case-insensitive title search",
                            "schema": { "type": "string", "maxLength": 160 }
                        },
                        {
                            "name": "provider",
                            "in": "query",
                            "required": false,
                            "schema": { "type": "string", "maxLength": 128 }
                        },
                        {
                            "name": "pinned",
                            "in": "query",
                            "required": false,
                            "schema": { "type": "boolean" }
                        },
                        {
                            "name": "state",
                            "in": "query",
                            "required": false,
                            "schema": {
                                "type": "string",
                                "enum": ["ready", "oversized", "scan_budget_exceeded", "unsupported_legacy", "invalid"]
                            }
                        }
                    ],
                    "responses": {
                        "200": {
                            "description": "Generation-consistent historical session page",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/SessionCatalogPage" }
                                }
                            }
                        },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "409": { "$ref": "#/components/responses/Conflict" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                }
            },
            "/sessions/{session_id}": {
                "get": {
                    "summary": "Get a local session handle",
                    "parameters": [{ "$ref": "#/components/parameters/SessionId" }],
                    "responses": {
                        "200": {
                            "description": "Session snapshot",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/SessionSnapshot" }
                                }
                            }
                        },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" }
                    }
                }
            },
            "/sessions/{session_id}/runs": {
                "post": {
                    "summary": "Start a run in a session",
                    "parameters": [{ "$ref": "#/components/parameters/SessionId" }],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/RunStartCommand" }
                            }
                        }
                    },
                    "responses": {
                        "201": {
                            "description": "Run-start command receipt",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/RunStartCommandReceipt" }
                                }
                            }
                        },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "409": { "$ref": "#/components/responses/Conflict" },
                        "500": { "$ref": "#/components/responses/InternalError" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                }
            },
            "/runs/{run_id}": {
                "get": {
                    "summary": "Get a run snapshot",
                    "parameters": [{ "$ref": "#/components/parameters/RunId" }],
                    "responses": {
                        "200": {
                            "description": "Run snapshot",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/RunSnapshot" }
                                }
                            }
                        },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" }
                    }
                }
            },
            "/runs/{run_id}/cancel": {
                "post": {
                    "summary": "Request run cancellation",
                    "parameters": [{ "$ref": "#/components/parameters/RunId" }],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/RunCancelCommand" }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Run-cancel command receipt",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/RunCancelCommandReceipt" }
                                }
                            }
                        },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "409": { "$ref": "#/components/responses/Conflict" },
                        "500": { "$ref": "#/components/responses/InternalError" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                }
            },
            "/runs/{run_id}/events": {
                "get": {
                    "summary": "Replay durable run events then follow live events",
                    "parameters": [
                        { "$ref": "#/components/parameters/RunId" },
                        {
                            "name": "Last-Event-ID",
                            "in": "header",
                            "required": false,
                            "schema": { "type": "string" }
                        }
                    ],
                    "responses": {
                        "200": {
                            "description": "Continuous text/event-stream until terminal, disconnect, lag, or shutdown",
                            "content": {
                                "text/event-stream": {
                                    "schema": { "type": "string" }
                                }
                            }
                        },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "409": { "$ref": "#/components/responses/Conflict" }
                    }
                }
            },
            "/runs/{run_id}/approvals/{call_id}": {
                "post": {
                    "summary": "Submit an approval decision for a pending tool call",
                    "parameters": [
                        { "$ref": "#/components/parameters/RunId" },
                        { "$ref": "#/components/parameters/CallId" }
                    ],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/ApprovalDecisionCommand" }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Approval command receipt",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/ApprovalCommandReceipt" }
                                }
                            }
                        },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "409": { "$ref": "#/components/responses/Conflict" },
                        "500": { "$ref": "#/components/responses/InternalError" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                }
            }
        },
        "components": {
            "securitySchemes": {
                "BearerAuth": {
                    "type": "http",
                    "scheme": "bearer"
                }
            },
            "parameters": {
                "SessionId": {
                    "name": "session_id",
                    "in": "path",
                    "required": true,
                    "schema": { "type": "string" }
                },
                "RunId": {
                    "name": "run_id",
                    "in": "path",
                    "required": true,
                    "schema": { "type": "string" }
                },
                "CallId": {
                    "name": "call_id",
                    "in": "path",
                    "required": true,
                    "schema": { "type": "string" }
                }
            },
            "responses": {
                "BadRequest": { "description": "Invalid request body or command payload", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } },
                "Unauthorized": { "description": "Bearer token is missing or invalid", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } },
                "NotFound": { "description": "Session, run, or route was not found", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } },
                "Conflict": { "description": "Command is stale, mismatched, expired, or not pending", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } },
                "InternalError": { "description": "Session binding, driver routing, or command completion failed", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } },
                "Unavailable": { "description": "The durable command identity store is unavailable or at capacity", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } }
            },
            "schemas": {
                "HealthResponse": {
                    "type": "object",
                    "required": ["status"],
                    "properties": { "status": { "type": "string", "const": "ok" } }
                },
                "ServerInfo": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["schema_version", "protocol_version", "server_version", "workspace_id", "bind_addr", "authentication", "shutdown_on_stdin_close", "capabilities"],
                    "properties": {
                        "schema_version": { "type": "integer", "const": 1 },
                        "protocol_version": { "type": "integer", "const": HTTP_PROTOCOL_VERSION },
                        "server_version": { "type": "string" },
                        "workspace_id": { "type": "string" },
                        "bind_addr": { "type": "string" },
                        "authentication": { "type": "string", "enum": ["bearer"] },
                        "shutdown_on_stdin_close": { "type": "boolean" },
                        "capabilities": { "$ref": "#/components/schemas/ServerCapabilities" }
                    }
                },
                "ServerCapabilities": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["session_catalog", "durable_session_reopen", "durable_event_replay", "live_events", "approval", "cancellation"],
                    "properties": {
                        "session_catalog": { "type": "boolean" },
                        "durable_session_reopen": { "type": "boolean" },
                        "durable_event_replay": { "type": "boolean" },
                        "live_events": { "type": "boolean" },
                        "approval": { "type": "boolean" },
                        "cancellation": { "type": "boolean" }
                    }
                },
                "SessionCreateRequest": {
                    "type": "object",
                    "properties": {
                        "label": { "type": "string" }
                    }
                },
                "SessionOpenRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["session_ref", "session_id"],
                    "properties": {
                        "session_ref": { "type": "string", "maxLength": 512, "pattern": "^[^/\\\\]+\\.jsonl$" },
                        "session_id": { "type": "string", "maxLength": 512 },
                        "label": { "type": ["string", "null"], "maxLength": 160 }
                    }
                },
                "SessionSnapshot": {
                    "type": "object",
                    "required": ["id", "run_ids", "durable_session_scope_id", "session_log_path"],
                    "properties": {
                        "id": { "type": "string" },
                        "label": { "type": ["string", "null"] },
                        "run_ids": { "type": "array", "items": { "type": "string" } },
                        "durable_session_scope_id": { "type": "string" },
                        "session_log_path": { "type": "string" },
                        "foreground_run_id": { "type": ["string", "null"] }
                    }
                },
                "SessionListResponse": {
                    "type": "object",
                    "required": ["sessions"],
                    "properties": {
                        "sessions": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/SessionSnapshot" }
                        }
                    }
                },
                "SessionCatalogPage": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["workspace_id", "generation", "reconciled_at_unix_ms", "degraded_source_count", "identity_conflict_count", "truncated_source_count", "entries"],
                    "properties": {
                        "workspace_id": { "type": "string" },
                        "generation": { "type": "integer", "format": "uint64" },
                        "reconciled_at_unix_ms": { "type": "integer", "format": "uint64" },
                        "degraded_source_count": { "type": "integer", "format": "uint64" },
                        "identity_conflict_count": { "type": "integer", "format": "uint64" },
                        "truncated_source_count": { "type": "integer", "format": "uint64" },
                        "entries": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/SessionCatalogEntry" }
                        },
                        "next_cursor": { "type": ["string", "null"] }
                    }
                },
                "SessionCatalogEntry": {
                    "type": "object",
                    "additionalProperties": false,
                    "description": "Compact historical metadata only; message and tool bodies are absent.",
                    "required": ["workspace_id", "session_ref", "source_state", "source_bytes", "source_modified_at_unix_ms", "user_message_count", "assistant_message_count", "tool_result_count", "control_entry_count", "pinned", "indexed_at_unix_ms"],
                    "properties": {
                        "workspace_id": { "type": "string" },
                        "session_ref": { "type": "string" },
                        "session_id": { "type": ["string", "null"] },
                        "source_state": {
                            "type": "string",
                            "enum": ["ready", "oversized", "scan_budget_exceeded", "unsupported_legacy", "invalid"]
                        },
                        "source_bytes": { "type": "integer", "format": "uint64" },
                        "source_modified_at_unix_ms": { "type": "integer", "format": "uint64" },
                        "provider_name": { "type": ["string", "null"] },
                        "model_name": { "type": ["string", "null"] },
                        "title": { "type": ["string", "null"] },
                        "user_message_count": { "type": "integer", "format": "uint64" },
                        "assistant_message_count": { "type": "integer", "format": "uint64" },
                        "tool_result_count": { "type": "integer", "format": "uint64" },
                        "control_entry_count": { "type": "integer", "format": "uint64" },
                        "pinned": { "type": "boolean" },
                        "indexed_at_unix_ms": { "type": "integer", "format": "uint64" }
                    }
                },
                "DisclosureListResponse": {
                    "type": "object",
                    "required": ["disclosures"],
                    "properties": {
                        "disclosures": {
                            "type": "array",
                            "items": { "type": "object" }
                        }
                    }
                },
                "CommandEnvelopeBase": {
                    "type": "object",
                    "required": ["protocol_version", "command_id", "client_id", "session_id", "payload"],
                    "properties": {
                        "protocol_version": { "type": "integer", "const": HTTP_PROTOCOL_VERSION },
                        "command_id": { "type": "string" },
                        "client_id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "expected_stream_sequence": { "type": ["integer", "null"], "format": "uint64" },
                        "correlation_id": { "type": ["string", "null"] }
                    }
                },
                "RunStartCommand": {
                    "allOf": [
                        { "$ref": "#/components/schemas/CommandEnvelopeBase" },
                        {
                            "type": "object",
                            "required": ["payload"],
                            "properties": {
                                "payload": { "$ref": "#/components/schemas/RunStartRequest" }
                            }
                        }
                    ]
                },
                "RunStartRequest": {
                    "type": "object",
                    "required": ["prompt", "approval_mode"],
                    "properties": {
                        "prompt": { "type": "string" },
                        "approval_mode": { "$ref": "#/components/schemas/RunApprovalMode" }
                    }
                },
                "RunApprovalMode": {
                    "type": "string",
                    "enum": ["ask", "allow_readonly", "deny"]
                },
                "RunStartCommandReceipt": {
                    "type": "object",
                    "required": ["command_id", "client_id", "session_id", "run", "replayed"],
                    "properties": {
                        "command_id": { "type": "string" },
                        "client_id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "expected_stream_sequence": { "type": ["integer", "null"], "format": "uint64" },
                        "correlation_id": { "type": ["string", "null"] },
                        "run": { "$ref": "#/components/schemas/RunSnapshot" },
                        "replayed": { "type": "boolean" }
                    }
                },
                "RunCancelCommand": {
                    "allOf": [
                        { "$ref": "#/components/schemas/CommandEnvelopeBase" },
                        {
                            "type": "object",
                            "required": ["payload"],
                            "properties": {
                                "payload": { "$ref": "#/components/schemas/RunCancelRequest" }
                            }
                        }
                    ]
                },
                "RunCancelRequest": {
                    "type": "object",
                    "properties": {
                        "reason": { "type": ["string", "null"] }
                    }
                },
                "RunCancelCommandReceipt": {
                    "type": "object",
                    "required": ["command_id", "client_id", "session_id", "run", "replayed"],
                    "properties": {
                        "command_id": { "type": "string" },
                        "client_id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "expected_stream_sequence": { "type": ["integer", "null"], "format": "uint64" },
                        "correlation_id": { "type": ["string", "null"] },
                        "run": { "$ref": "#/components/schemas/RunSnapshot" },
                        "replayed": { "type": "boolean" }
                    }
                },
                "RunSnapshot": {
                    "type": "object",
                    "required": ["id", "session_id", "status", "approval_mode", "prompt_preview", "pending_approval_call_ids", "stream_sequence"],
                    "properties": {
                        "id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "status": { "$ref": "#/components/schemas/RunStatus" },
                        "approval_mode": { "$ref": "#/components/schemas/RunApprovalMode" },
                        "prompt_preview": { "type": "string" },
                        "stream_sequence": { "type": "integer", "format": "uint64" },
                        "pending_approval_call_ids": { "type": "array", "items": { "type": "string" } }
                    }
                },
                "RunStatus": {
                    "type": "string",
                    "enum": ["starting", "running", "waiting_for_approval", "cancel_requested", "execution_uncertain", "finished", "failed", "cancelled", "interrupted"]
                },
                "ApprovalDecisionCommand": {
                    "allOf": [
                        { "$ref": "#/components/schemas/CommandEnvelopeBase" },
                        {
                            "type": "object",
                            "required": ["payload"],
                            "properties": {
                                "payload": { "$ref": "#/components/schemas/ApprovalDecisionRequest" }
                            }
                        }
                    ]
                },
                "ApprovalDecisionRequest": {
                    "type": "object",
                    "required": ["approval_request_id", "tool_call_hash", "policy_version", "expires_at_ms", "decision"],
                    "properties": {
                        "approval_request_id": { "type": "string" },
                        "tool_call_hash": { "type": "string" },
                        "policy_version": { "type": "string" },
                        "expires_at_ms": { "type": "integer", "format": "uint64" },
                        "decision": { "$ref": "#/components/schemas/ApprovalDecision" },
                        "reason": { "type": ["string", "null"] }
                    }
                },
                "ApprovalDecision": {
                    "type": "string",
                    "enum": ["approve", "deny"]
                },
                "ApprovalCommandReceipt": {
                    "type": "object",
                    "required": ["command_id", "client_id", "session_id", "run_id", "call_id", "decision", "replayed"],
                    "properties": {
                        "command_id": { "type": "string" },
                        "client_id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "run_id": { "type": "string" },
                        "call_id": { "type": "string" },
                        "expected_stream_sequence": { "type": ["integer", "null"], "format": "uint64" },
                        "correlation_id": { "type": ["string", "null"] },
                        "decision": { "$ref": "#/components/schemas/ApprovalDecisionRecord" },
                        "replayed": { "type": "boolean" }
                    }
                },
                "ApprovalDecisionRecord": {
                    "type": "object",
                    "required": ["run_id", "call_id", "decision"],
                    "properties": {
                        "run_id": { "type": "string" },
                        "call_id": { "type": "string" },
                        "decision": { "type": "string", "enum": ["approved", "denied"] },
                        "reason": { "type": ["string", "null"] }
                    }
                },
                "ErrorResponse": {
                    "type": "object",
                    "required": ["error"],
                    "properties": {
                        "error": {
                            "type": "object",
                            "required": ["code", "message"],
                            "properties": {
                                "code": { "type": "string" },
                                "message": { "type": "string" }
                            }
                        }
                    }
                }
            }
        }
    })
}
