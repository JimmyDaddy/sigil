use serde_json::{Value, json};

use crate::{HTTP_SERVER_INFO_SCHEMA_VERSION, protocol::HTTP_PROTOCOL_VERSION};

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
            "/session-catalog/rename": {
                "post": {
                    "summary": "Rename one exact durable conversation",
                    "description": "Appends a bounded display-name decision to workspace lifecycle truth, then refreshes the rebuildable catalog projection.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SessionRenameRequest" } } }
                    },
                    "responses": {
                        "200": { "description": "Committed rename receipt", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SessionMutationReceipt" } } } },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "409": { "$ref": "#/components/responses/Conflict" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                }
            },
            "/session-catalog/delete": {
                "post": {
                    "summary": "Delete one exact durable conversation",
                    "description": "Rejects pinned or active sessions, then applies the existing content-bound preview/delete lifecycle and evicts idle adapter handles.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SessionDeleteRequest" } } }
                    },
                    "responses": {
                        "200": { "description": "Committed delete receipt", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SessionMutationReceipt" } } } },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "409": { "$ref": "#/components/responses/Conflict" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                }
            },
            "/session-catalog/quarantine": {
                "post": {
                    "summary": "Quarantine one exact invalid local session source",
                    "description": "Revalidates the invalid source metadata under a maintenance lease, then moves it into the local quarantine directory without exposing a filesystem path.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SessionQuarantineRequest" } } }
                    },
                    "responses": {
                        "200": { "description": "Committed quarantine receipt", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SessionQuarantineReceipt" } } } },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
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
            "/sessions/{session_id}/transcript": {
                "get": {
                    "summary": "Read one bounded chronological page of durable conversation messages",
                    "description": "Projects user, assistant and tool-result text from scope-checked append-only session truth. System/control entries, tool arguments, resolved image bytes and server-private paths are excluded.",
                    "parameters": [
                        { "$ref": "#/components/parameters/SessionId" },
                        {
                            "name": "limit",
                            "in": "query",
                            "required": false,
                            "schema": { "type": "integer", "minimum": 1, "maximum": 100, "default": 50 }
                        },
                        {
                            "name": "before",
                            "in": "query",
                            "required": false,
                            "description": "Exclusive one-based message ordinal for the next older page",
                            "schema": { "type": "integer", "format": "uint64", "minimum": 1 }
                        }
                    ],
                    "responses": {
                        "200": {
                            "description": "Bounded transcript page in chronological order",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SessionTranscriptPage" } } }
                        },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "500": { "$ref": "#/components/responses/InternalError" }
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
            "/sessions/{session_id}/run-context": {
                "get": {
                    "summary": "Read typed model, approval-mode, and context usage facts",
                    "description": "Projects the durable session model identity and latest provider usage without exposing server-private paths or inventing missing context values.",
                    "parameters": [{ "$ref": "#/components/parameters/SessionId" }],
                    "responses": {
                        "200": {
                            "description": "Typed run context for the next run",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/RunContextView" } } }
                        },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/sessions/{session_id}/verification": {
                "get": {
                    "summary": "Project the current task verification recommendation and evidence",
                    "parameters": [{ "$ref": "#/components/parameters/SessionId" }],
                    "responses": {
                        "200": {
                            "description": "Shared verification product projection",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/VerificationView" } } }
                        },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/sessions/{session_id}/verification/rerun": {
                "post": {
                    "summary": "Rerun one exact stale-safe recommended verification check",
                    "parameters": [{ "$ref": "#/components/parameters/SessionId" }],
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/VerificationRerunCommand" } } }
                    },
                    "responses": {
                        "200": {
                            "description": "Durable verification rerun receipt",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/VerificationRerunCommandReceipt" } } }
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
                        "schema_version": { "type": "integer", "const": HTTP_SERVER_INFO_SCHEMA_VERSION },
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
                    "required": ["session_catalog", "durable_session_reopen", "bounded_transcript_replay", "durable_event_replay", "live_events", "approval", "cancellation", "verification", "run_context"],
                    "properties": {
                        "session_catalog": { "type": "boolean" },
                        "durable_session_reopen": { "type": "boolean" },
                        "bounded_transcript_replay": { "type": "boolean" },
                        "durable_event_replay": { "type": "boolean" },
                        "live_events": { "type": "boolean" },
                        "approval": { "type": "boolean" },
                        "cancellation": { "type": "boolean" },
                        "verification": { "type": "boolean" },
                        "run_context": { "type": "boolean" }
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
                "SessionRenameRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["session_ref", "session_id", "display_name"],
                    "properties": {
                        "session_ref": { "type": "string", "maxLength": 128, "pattern": "^[^/\\\\]+\\.jsonl$" },
                        "session_id": { "type": "string", "maxLength": 512 },
                        "display_name": { "type": "string", "minLength": 1, "maxLength": 160 }
                    }
                },
                "SessionDeleteRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["session_ref", "session_id"],
                    "properties": {
                        "session_ref": { "type": "string", "maxLength": 128, "pattern": "^[^/\\\\]+\\.jsonl$" },
                        "session_id": { "type": "string", "maxLength": 512 }
                    }
                },
                "SessionQuarantineRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["session_ref", "source_bytes", "source_modified_at_unix_ms"],
                    "properties": {
                        "session_ref": { "type": "string", "maxLength": 128, "pattern": "^[^/\\\\]+\\.jsonl$" },
                        "source_bytes": { "type": "integer", "format": "uint64" },
                        "source_modified_at_unix_ms": { "type": "integer", "format": "uint64" }
                    }
                },
                "SessionMutationReceipt": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["session_ref", "session_id", "operation_id"],
                    "properties": {
                        "session_ref": { "type": "string" },
                        "session_id": { "type": "string" },
                        "operation_id": { "type": "string" },
                        "projection_generation": { "type": ["integer", "null"], "format": "uint64" }
                    }
                },
                "SessionQuarantineReceipt": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["session_ref", "operation_id", "quarantine_name"],
                    "properties": {
                        "session_ref": { "type": "string" },
                        "operation_id": { "type": "string" },
                        "quarantine_name": { "type": "string" },
                        "projection_generation": { "type": ["integer", "null"], "format": "uint64" }
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
                "SessionTranscriptPage": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["session_scope_id", "total_messages", "messages"],
                    "properties": {
                        "session_scope_id": { "type": "string" },
                        "total_messages": { "type": "integer", "format": "uint64" },
                        "messages": {
                            "type": "array",
                            "maxItems": 100,
                            "items": { "$ref": "#/components/schemas/SessionTranscriptMessage" }
                        },
                        "next_before": { "type": ["integer", "null"], "format": "uint64", "minimum": 1 }
                    }
                },
                "SessionTranscriptMessage": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["ordinal", "message_id", "role", "image_attachment_count", "truncated", "original_content_bytes"],
                    "properties": {
                        "ordinal": { "type": "integer", "format": "uint64", "minimum": 1 },
                        "message_id": { "type": "string" },
                        "role": { "type": "string", "enum": ["user", "assistant", "tool"] },
                        "content": { "type": ["string", "null"], "maxLength": 65536 },
                        "assistant_kind": {
                            "type": ["string", "null"],
                            "enum": ["tool_preamble", "progress", "reasoning_trace", "final_answer", null]
                        },
                        "tool_name": { "type": ["string", "null"], "maxLength": 128 },
                        "image_attachment_count": { "type": "integer", "format": "uint64" },
                        "truncated": { "type": "boolean" },
                        "original_content_bytes": { "type": "integer", "format": "uint64" }
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
                "RunContextView": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["provider_name", "model_name", "model_selection", "default_approval_mode", "available_approval_modes", "context_window_source"],
                    "properties": {
                        "provider_name": { "type": "string" },
                        "model_name": { "type": "string" },
                        "model_selection": { "type": "string", "enum": ["fixed_for_session"] },
                        "default_approval_mode": { "$ref": "#/components/schemas/RunApprovalMode" },
                        "available_approval_modes": {
                            "type": "array",
                            "minItems": 1,
                            "uniqueItems": true,
                            "items": { "$ref": "#/components/schemas/RunApprovalMode" }
                        },
                        "context_window_tokens": { "type": ["integer", "null"], "format": "uint32" },
                        "last_prompt_tokens": { "type": ["integer", "null"], "format": "uint64" },
                        "context_window_source": { "type": "string", "enum": ["provider", "config", "unavailable"] }
                    }
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
                "VerificationRerunCommand": {
                    "allOf": [
                        { "$ref": "#/components/schemas/CommandEnvelopeBase" },
                        {
                            "type": "object",
                            "required": ["payload"],
                            "properties": {
                                "payload": { "$ref": "#/components/schemas/VerificationRerunRequest" }
                            }
                        }
                    ]
                },
                "VerificationRerunRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["task_id", "step_id", "check_spec_id", "check_spec_hash", "policy_hash", "workspace_snapshot_id"],
                    "properties": {
                        "task_id": { "type": "string" },
                        "step_id": { "type": "string" },
                        "check_spec_id": { "type": "string" },
                        "check_spec_hash": { "type": "string" },
                        "policy_hash": { "type": "string" },
                        "workspace_snapshot_id": { "type": "string" }
                    }
                },
                "VerificationRerunCommandReceipt": {
                    "type": "object",
                    "required": ["command_id", "client_id", "session_id", "verification", "replayed"],
                    "properties": {
                        "command_id": { "type": "string" },
                        "client_id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "correlation_id": { "type": ["string", "null"] },
                        "verification": { "$ref": "#/components/schemas/VerificationView" },
                        "replayed": { "type": "boolean" }
                    }
                },
                "VerificationView": {
                    "type": "object",
                    "required": ["task_id", "step_id", "scope", "verdict", "status", "recommended_check_spec_id", "recommendation_kind", "recommendation_reason", "action", "evidence"],
                    "properties": {
                        "task_id": { "type": "string" },
                        "step_id": { "type": "string" },
                        "scope": { "$ref": "#/components/schemas/EvidenceScope" },
                        "verdict": { "$ref": "#/components/schemas/VerificationVerdict" },
                        "status": { "type": "string" },
                        "recommended_check_spec_id": { "type": ["string", "null"] },
                        "recommendation_kind": {
                            "oneOf": [
                                { "$ref": "#/components/schemas/VerificationRecommendationKind" },
                                { "type": "null" }
                            ]
                        },
                        "recommendation_reason": { "type": ["string", "null"] },
                        "action": {
                            "oneOf": [
                                { "$ref": "#/components/schemas/VerificationRerunAction" },
                                { "$ref": "#/components/schemas/VerificationReviewApprovalAction" },
                                { "type": "null" }
                            ]
                        },
                        "evidence": { "$ref": "#/components/schemas/VerificationEvidence" }
                    }
                },
                "VerificationRecommendationKind": {
                    "type": "string",
                    "enum": ["run", "rerun_non_writing", "retry", "review_approval"]
                },
                "VerificationRerunAction": {
                    "type": "object",
                    "required": ["kind", "request"],
                    "properties": {
                        "kind": { "type": "string", "const": "rerun" },
                        "request": { "$ref": "#/components/schemas/VerificationRerunRequest" }
                    }
                },
                "VerificationReviewApprovalAction": {
                    "type": "object",
                    "required": ["kind", "request"],
                    "properties": {
                        "kind": { "type": "string", "const": "review_approval" },
                        "request": {
                            "type": "object",
                            "required": ["check_spec_id"],
                            "properties": { "check_spec_id": { "type": "string" } }
                        }
                    }
                },
                "EvidenceScope": {
                    "type": "object",
                    "required": ["kind", "id"],
                    "properties": {
                        "kind": { "type": "string", "enum": ["run", "workspace", "task", "step", "agent", "changeset"] },
                        "id": { "type": "string" }
                    }
                },
                "VerificationVerdict": {
                    "type": "string",
                    "enum": ["not_evaluated", "not_applicable", "pending", "passed", "failed", "missing", "inconclusive", "stale", "skipped"]
                },
                "VerificationEvidence": {
                    "type": "object",
                    "required": ["check_run_id", "check_spec_id", "check_status", "receipt_id", "workspace_snapshot_id", "changeset_id", "changeset_apply_event_id", "command_event_id", "output_artifact_id", "failure_summary"],
                    "properties": {
                        "check_run_id": { "type": ["string", "null"] },
                        "check_spec_id": { "type": ["string", "null"] },
                        "check_status": { "type": ["string", "null"], "enum": ["queued", "running", "succeeded", "failed", "skipped", "inconclusive", "errored", null] },
                        "receipt_id": { "type": ["string", "null"] },
                        "workspace_snapshot_id": { "type": ["string", "null"] },
                        "changeset_id": { "type": ["string", "null"] },
                        "changeset_apply_event_id": { "type": ["string", "null"] },
                        "command_event_id": { "type": ["string", "null"] },
                        "output_artifact_id": { "type": ["string", "null"] },
                        "failure_summary": { "type": ["string", "null"] }
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
