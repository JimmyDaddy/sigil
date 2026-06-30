use serde_json::{Value, json};

use crate::protocol::HTTP_PROTOCOL_VERSION;

/// OpenAPI version emitted for the MVP desktop/app-server command surface.
pub const HTTP_OPENAPI_VERSION: &str = "3.1.0";

/// Returns the MVP OpenAPI description for the local HTTP command surface.
///
/// The document intentionally covers only routes implemented by this crate:
/// health, session creation, run start, and approval decision submission.
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
            "/sessions": {
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
                        "401": { "$ref": "#/components/responses/Unauthorized" }
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
                        "409": { "$ref": "#/components/responses/Conflict" }
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
                "Conflict": { "description": "Command is stale, mismatched, expired, or not pending", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } }
            },
            "schemas": {
                "HealthResponse": {
                    "type": "object",
                    "required": ["status"],
                    "properties": { "status": { "type": "string", "const": "ok" } }
                },
                "SessionCreateRequest": {
                    "type": "object",
                    "properties": {
                        "label": { "type": "string" }
                    }
                },
                "SessionSnapshot": {
                    "type": "object",
                    "required": ["id", "label", "created_at_ms", "run_ids"],
                    "properties": {
                        "id": { "type": "string" },
                        "label": { "type": ["string", "null"] },
                        "created_at_ms": { "type": "integer", "format": "uint64" },
                        "run_ids": { "type": "array", "items": { "type": "string" } }
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
                "RunSnapshot": {
                    "type": "object",
                    "required": ["id", "session_id", "status", "prompt", "approval_mode", "created_at_ms", "updated_at_ms", "stream_sequence", "pending_approval_call_ids"],
                    "properties": {
                        "id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "status": { "$ref": "#/components/schemas/RunStatus" },
                        "prompt": { "type": "string" },
                        "approval_mode": { "$ref": "#/components/schemas/RunApprovalMode" },
                        "created_at_ms": { "type": "integer", "format": "uint64" },
                        "updated_at_ms": { "type": "integer", "format": "uint64" },
                        "stream_sequence": { "type": "integer", "format": "uint64" },
                        "pending_approval_call_ids": { "type": "array", "items": { "type": "string" } }
                    }
                },
                "RunStatus": {
                    "type": "string",
                    "enum": ["starting", "running", "waiting_for_approval", "completed", "cancelled", "failed"]
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
