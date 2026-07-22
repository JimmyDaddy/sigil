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
            "/support/doctor": {
                "get": {
                    "summary": "Read redacted local diagnostics",
                    "description": "Returns only the frozen path-free support projection. Credentials, local paths, conversation content, tool payloads, and file content are excluded.",
                    "responses": {
                        "200": { "description": "Redacted diagnostic report", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SupportDoctorReport" } } } },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                }
            },
            "/support/bundle": {
                "post": {
                    "summary": "Build a private redacted support bundle",
                    "description": "Returns bounded JSON only to the native desktop client. The renderer does not receive the bundle content or a filesystem path.",
                    "responses": {
                        "200": { "description": "Private bounded support bundle", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SupportBundleExport" } } } },
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
            "/session-catalog/batch/plan": {
                "post": {
                    "summary": "Preview one exact bounded session catalog batch",
                    "description": "Reconciles current durable catalog truth, classifies each selected identity, and returns a content-bound plan without mutating any session source.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SessionCatalogBatchPlanRequest" } } }
                    },
                    "responses": {
                        "200": { "description": "Content-bound batch preview", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SessionCatalogBatchPlan" } } } },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                }
            },
            "/session-catalog/batch/execute": {
                "post": {
                    "summary": "Execute one confirmed session catalog batch",
                    "description": "Replans and compares the opaque plan digest before the first mutation, then returns a per-item best-effort receipt. The operation is not an atomic transaction across session files.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SessionCatalogBatchExecuteRequest" } } }
                    },
                    "responses": {
                        "200": { "description": "Per-item best-effort batch receipt", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SessionCatalogBatchReceipt" } } } },
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
            "/session-catalog/delete-invalid-source": {
                "post": {
                    "summary": "Permanently delete one exact invalid local session source",
                    "description": "Revalidates the invalid source fingerprint under a maintenance lease, then permanently removes the regular file after native-shell confirmation.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SessionInvalidSourceDeleteRequest" } } }
                    },
                    "responses": {
                        "200": { "description": "Committed invalid-source delete receipt", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SessionInvalidSourceDeleteReceipt" } } } },
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
            "/sessions/{session_id}/continuity": {
                "get": {
                    "summary": "Probe durable frontier and current foreground ownership",
                    "description": "Revalidates the durable session frontier and returns one nested process-local foreground owner with an opaque revision for exact attach admission.",
                    "parameters": [{ "$ref": "#/components/parameters/SessionId" }],
                    "responses": {
                        "200": {
                            "description": "Fresh conversation continuity proof",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SessionContinuityView" } } }
                        },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "500": { "$ref": "#/components/responses/InternalError" }
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
                    "summary": "Read typed model, permission-mode, and context usage facts",
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
            "/sessions/{session_id}/agent-activity": {
                "get": {
                    "summary": "Read bounded child-agent lifecycle and result handoff state",
                    "description": "Projects safe child-agent status, objective, bounded result summary and usage. Child session references, paths, hashes and raw tool arguments are excluded.",
                    "parameters": [{ "$ref": "#/components/parameters/SessionId" }],
                    "responses": {
                        "200": {
                            "description": "Newest child-agent activity first",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/AgentActivityView" } } }
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
                            "name": "X-Sigil-Session-Id",
                            "in": "header",
                            "required": true,
                            "schema": { "type": "string", "maxLength": 512 }
                        },
                        {
                            "name": "X-Sigil-Owner-Revision",
                            "in": "header",
                            "required": true,
                            "schema": { "type": "string", "pattern": "^sha256:[0-9a-f]{64}$" }
                        },
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
                    "required": ["session_catalog", "durable_session_reopen", "bounded_transcript_replay", "durable_event_replay", "live_events", "approval", "cancellation", "verification", "run_context", "agent_activity", "support_diagnostics"],
                    "properties": {
                        "session_catalog": { "type": "boolean" },
                        "durable_session_reopen": { "type": "boolean" },
                        "bounded_transcript_replay": { "type": "boolean" },
                        "durable_event_replay": { "type": "boolean" },
                        "live_events": { "type": "boolean" },
                        "approval": { "type": "boolean" },
                        "cancellation": { "type": "boolean" },
                        "verification": { "type": "boolean" },
                        "run_context": { "type": "boolean" },
                        "agent_activity": { "type": "boolean" },
                        "support_diagnostics": { "type": "boolean" }
                    }
                },
                "SupportStatus": {
                    "type": "string",
                    "enum": ["ok", "warn", "error"]
                },
                "SupportSummary": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["overall_status", "ok", "warn", "error"],
                    "properties": {
                        "overall_status": { "$ref": "#/components/schemas/SupportStatus" },
                        "ok": { "type": "integer", "format": "uint64" },
                        "warn": { "type": "integer", "format": "uint64" },
                        "error": { "type": "integer", "format": "uint64" }
                    }
                },
                "SupportCheck": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["status", "name", "summary"],
                    "properties": {
                        "status": { "$ref": "#/components/schemas/SupportStatus" },
                        "name": { "type": "string" },
                        "summary": { "type": "string" },
                        "remediation": { "type": ["string", "null"] }
                    }
                },
                "SupportEnvironment": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["os", "architecture", "terminal_family"],
                    "properties": {
                        "os": { "type": "string" },
                        "architecture": { "type": "string" },
                        "terminal_family": { "type": "string", "enum": ["iterm2", "apple_terminal", "wezterm", "vscode", "other", "unknown"] }
                    }
                },
                "SupportPrivacy": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["included", "excluded", "review_before_sharing"],
                    "properties": {
                        "included": { "type": "array", "items": { "type": "string" } },
                        "excluded": { "type": "array", "items": { "type": "string" } },
                        "review_before_sharing": { "type": "boolean" }
                    }
                },
                "SupportDoctorReport": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["generated_at_unix_ms", "version", "commit", "target", "profile", "environment", "summary", "checks", "privacy"],
                    "properties": {
                        "generated_at_unix_ms": { "type": "integer", "format": "uint64" },
                        "version": { "type": "string" },
                        "commit": { "type": "string" },
                        "target": { "type": "string" },
                        "profile": { "type": "string" },
                        "environment": { "$ref": "#/components/schemas/SupportEnvironment" },
                        "summary": { "$ref": "#/components/schemas/SupportSummary" },
                        "checks": { "type": "array", "items": { "$ref": "#/components/schemas/SupportCheck" } },
                        "privacy": { "$ref": "#/components/schemas/SupportPrivacy" }
                    }
                },
                "SupportBundleExport": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["suggested_file_name", "generated_at_unix_ms", "content"],
                    "properties": {
                        "suggested_file_name": { "type": "string" },
                        "generated_at_unix_ms": { "type": "integer", "format": "uint64" },
                        "content": { "type": "string", "maxLength": 262144 }
                    }
                },
                "SessionCreateRequest": {
                    "type": "object",
                    "properties": {
                        "label": { "type": "string" },
                        "model_name": { "type": "string" }
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
                "SessionInvalidSourceDeleteRequest": {
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
                "SessionInvalidSourceDeleteReceipt": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["session_ref", "operation_id"],
                    "properties": {
                        "session_ref": { "type": "string" },
                        "operation_id": { "type": "string" },
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
                "DurableSessionFrontier": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["through_stream_sequence"],
                    "properties": {
                        "through_stream_sequence": { "type": "integer", "format": "uint64" }
                    }
                },
                "ForegroundRunOwner": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["run_id", "owner_revision"],
                    "properties": {
                        "run_id": { "type": "string" },
                        "owner_revision": { "type": "string", "pattern": "^sha256:[0-9a-f]{64}$" }
                    }
                },
                "ContinuityRecoveryAction": {
                    "type": "string",
                    "enum": ["retry_current", "open_another_workspace", "open_diagnostics", "show_details", "continue_read_only"]
                },
                "SessionContinuityView": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["durable_session_scope_id", "durable_frontier", "recovery_actions"],
                    "properties": {
                        "durable_session_scope_id": { "type": "string" },
                        "durable_frontier": { "$ref": "#/components/schemas/DurableSessionFrontier" },
                        "foreground_owner": {
                            "anyOf": [
                                { "$ref": "#/components/schemas/ForegroundRunOwner" },
                                { "type": "null" }
                            ]
                        },
                        "recovery_actions": {
                            "type": "array",
                            "maxItems": 5,
                            "uniqueItems": true,
                            "items": { "$ref": "#/components/schemas/ContinuityRecoveryAction" }
                        }
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
                "AgentActivityStatus": {
                    "type": "string",
                    "enum": ["started", "running", "blocked", "completed", "failed", "cancelled", "interrupted", "unavailable", "unknown"]
                },
                "AgentHandoffStatus": {
                    "type": "string",
                    "enum": ["pending", "result_ready", "result_read", "returned", "unavailable"]
                },
                "AgentUsageSummary": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["input_tokens", "output_tokens", "total_tokens"],
                    "properties": {
                        "input_tokens": { "type": "integer", "format": "uint64" },
                        "output_tokens": { "type": "integer", "format": "uint64" },
                        "total_tokens": { "type": "integer", "format": "uint64" },
                        "cached_tokens": { "type": ["integer", "null"], "format": "uint64" }
                    }
                },
                "AgentActivityItem": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["thread_id", "objective", "status", "handoff_status", "result_summary_truncated"],
                    "properties": {
                        "thread_id": { "type": "string" },
                        "profile_id": { "type": ["string", "null"] },
                        "display_name": { "type": ["string", "null"], "maxLength": 32768 },
                        "objective": { "type": "string", "maxLength": 32768 },
                        "status": { "$ref": "#/components/schemas/AgentActivityStatus" },
                        "reason": { "type": ["string", "null"], "maxLength": 32768 },
                        "handoff_status": { "$ref": "#/components/schemas/AgentHandoffStatus" },
                        "result_summary": { "type": ["string", "null"], "maxLength": 32768 },
                        "result_summary_truncated": { "type": "boolean" },
                        "usage": { "oneOf": [{ "$ref": "#/components/schemas/AgentUsageSummary" }, { "type": "null" }] }
                    }
                },
                "AgentActivityView": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["total_agents", "active_agents", "terminal_agents", "items"],
                    "properties": {
                        "total_agents": { "type": "integer", "format": "uint64" },
                        "active_agents": { "type": "integer", "format": "uint64" },
                        "terminal_agents": { "type": "integer", "format": "uint64" },
                        "items": { "type": "array", "maxItems": 100, "items": { "$ref": "#/components/schemas/AgentActivityItem" } }
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
                "SessionCatalogBatchAction": {
                    "type": "string",
                    "enum": ["delete_sessions", "quarantine_invalid_sources", "delete_invalid_sources"]
                },
                "SessionCatalogBatchItem": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["session_ref"],
                    "properties": {
                        "session_ref": { "type": "string", "maxLength": 512 },
                        "session_id": { "type": ["string", "null"], "maxLength": 512 },
                        "source_bytes": { "type": ["integer", "null"], "format": "uint64" },
                        "source_modified_at_unix_ms": { "type": ["integer", "null"], "format": "uint64" }
                    }
                },
                "SessionCatalogBatchPlanRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["action", "items"],
                    "properties": {
                        "action": { "$ref": "#/components/schemas/SessionCatalogBatchAction" },
                        "items": { "type": "array", "minItems": 1, "maxItems": 100, "items": { "$ref": "#/components/schemas/SessionCatalogBatchItem" } }
                    }
                },
                "SessionCatalogBatchExecuteRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["plan_id", "action", "items"],
                    "properties": {
                        "plan_id": { "type": "string", "maxLength": 128 },
                        "action": { "$ref": "#/components/schemas/SessionCatalogBatchAction" },
                        "items": { "type": "array", "minItems": 1, "maxItems": 100, "items": { "$ref": "#/components/schemas/SessionCatalogBatchItem" } }
                    }
                },
                "SessionCatalogBatchPlanItem": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["session_ref", "status"],
                    "properties": {
                        "session_ref": { "type": "string" },
                        "status": { "type": "string", "enum": ["executable", "blocked"] },
                        "reason": { "type": ["string", "null"] }
                    }
                },
                "SessionCatalogBatchPlan": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["plan_id", "action", "generation", "total", "executable", "blocked", "items"],
                    "properties": {
                        "plan_id": { "type": "string" },
                        "action": { "$ref": "#/components/schemas/SessionCatalogBatchAction" },
                        "generation": { "type": "integer", "format": "uint64" },
                        "total": { "type": "integer", "format": "uint64" },
                        "executable": { "type": "integer", "format": "uint64" },
                        "blocked": { "type": "integer", "format": "uint64" },
                        "items": { "type": "array", "items": { "$ref": "#/components/schemas/SessionCatalogBatchPlanItem" } }
                    }
                },
                "SessionCatalogBatchReceiptItem": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["session_ref", "outcome"],
                    "properties": {
                        "session_ref": { "type": "string" },
                        "outcome": { "type": "string", "enum": ["completed", "failed", "skipped"] },
                        "reason": { "type": ["string", "null"] },
                        "operation_id": { "type": ["string", "null"] },
                        "quarantine_name": { "type": ["string", "null"] },
                        "projection_generation": { "type": ["integer", "null"], "format": "uint64" }
                    }
                },
                "SessionCatalogBatchReceipt": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["plan_id", "action", "total", "completed", "failed", "skipped", "items"],
                    "properties": {
                        "plan_id": { "type": "string" },
                        "action": { "$ref": "#/components/schemas/SessionCatalogBatchAction" },
                        "total": { "type": "integer", "format": "uint64" },
                        "completed": { "type": "integer", "format": "uint64" },
                        "failed": { "type": "integer", "format": "uint64" },
                        "skipped": { "type": "integer", "format": "uint64" },
                        "items": { "type": "array", "items": { "$ref": "#/components/schemas/SessionCatalogBatchReceiptItem" } }
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
                    "required": ["prompt", "permission_mode"],
                    "properties": {
                        "prompt": { "type": "string" },
                        "permission_mode": { "$ref": "#/components/schemas/PermissionMode" },
                        "model_name": { "type": ["string", "null"] },
                        "model_selection_binding": { "type": ["string", "null"] },
                        "reasoning_effort": { "oneOf": [{ "$ref": "#/components/schemas/ReasoningEffort" }, { "type": "null" }] },
                        "reasoning_effort_binding": { "type": ["string", "null"] },
                        "skill_binding": { "oneOf": [{ "$ref": "#/components/schemas/ApplicationSkillBinding" }, { "type": "null" }] },
                        "agent_binding": { "oneOf": [{ "$ref": "#/components/schemas/ApplicationAgentBinding" }, { "type": "null" }] }
                    }
                },
                "PermissionMode": {
                    "type": "string",
                    "enum": ["read-only", "manual", "auto-edit", "danger-full-access"]
                },
                "ReasoningEffort": {
                    "type": "string",
                    "enum": ["low", "medium", "high", "max"]
                },
                "ApplicationModelOption": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["model_name", "available_reasoning_efforts"],
                    "properties": {
                        "model_name": { "type": "string" },
                        "available_reasoning_efforts": {
                            "type": "array",
                            "uniqueItems": true,
                            "items": { "$ref": "#/components/schemas/ReasoningEffort" }
                        },
                        "default_reasoning_effort": { "oneOf": [{ "$ref": "#/components/schemas/ReasoningEffort" }, { "type": "null" }] },
                        "reasoning_effort_binding": { "type": ["string", "null"] }
                    }
                },
                "RunContextView": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["provider_name", "model_name", "available_models", "model_options", "model_selection", "model_selection_binding", "default_permission_mode", "available_permission_modes", "available_reasoning_efforts", "context_window_source", "extension_catalog"],
                    "properties": {
                        "provider_name": { "type": "string" },
                        "model_name": { "type": "string" },
                        "available_models": {
                            "type": "array",
                            "minItems": 1,
                            "uniqueItems": true,
                            "items": { "type": "string" }
                        },
                        "model_options": {
                            "type": "array",
                            "minItems": 1,
                            "items": { "$ref": "#/components/schemas/ApplicationModelOption" }
                        },
                        "model_selection": { "type": "string", "enum": ["per_run"] },
                        "model_selection_binding": { "type": "string" },
                        "default_permission_mode": { "$ref": "#/components/schemas/PermissionMode" },
                        "available_permission_modes": {
                            "type": "array",
                            "minItems": 1,
                            "uniqueItems": true,
                            "items": { "$ref": "#/components/schemas/PermissionMode" }
                        },
                        "available_reasoning_efforts": {
                            "type": "array",
                            "uniqueItems": true,
                            "items": { "$ref": "#/components/schemas/ReasoningEffort" }
                        },
                        "default_reasoning_effort": { "oneOf": [{ "$ref": "#/components/schemas/ReasoningEffort" }, { "type": "null" }] },
                        "reasoning_effort_binding": { "type": ["string", "null"] },
                        "context_window_tokens": { "type": ["integer", "null"], "format": "uint32" },
                        "last_prompt_tokens": { "type": ["integer", "null"], "format": "uint64" },
                        "context_window_source": { "type": "string", "enum": ["provider", "config", "unavailable"] },
                        "extension_catalog": { "$ref": "#/components/schemas/ApplicationExtensionCatalog" }
                    }
                },
                "ApplicationExtensionCatalog": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["commands", "skills", "agents"],
                    "properties": {
                        "commands": { "type": "array", "items": { "$ref": "#/components/schemas/ApplicationCommandCatalogEntry" } },
                        "skills": { "type": "array", "items": { "$ref": "#/components/schemas/ApplicationSkillCatalogEntry" } },
                        "agents": { "type": "array", "items": { "$ref": "#/components/schemas/ApplicationAgentCatalogEntry" } }
                    }
                },
                "ApplicationClientAction": {
                    "type": "string",
                    "enum": ["new_session", "focus_effort", "focus_model", "open_session_picker", "open_agent_workbench", "open_settings", "open_support"]
                },
                "ApplicationCommandCatalogEntry": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["canonical", "aliases", "label", "description", "completes_with_space", "available"],
                    "properties": {
                        "canonical": { "type": "string" },
                        "aliases": { "type": "array", "items": { "type": "string" } },
                        "label": { "type": "string" },
                        "description": { "type": "string" },
                        "argument_hint": { "type": ["string", "null"] },
                        "completes_with_space": { "type": "boolean" },
                        "client_action": { "oneOf": [{ "$ref": "#/components/schemas/ApplicationClientAction" }, { "type": "null" }] },
                        "available": { "type": "boolean" },
                        "unavailable_reason": { "type": ["string", "null"] }
                    }
                },
                "ApplicationSkillBinding": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["skill_id", "skill_sha256", "index_fingerprint"],
                    "properties": {
                        "skill_id": { "type": "string" },
                        "skill_sha256": { "type": "string" },
                        "index_fingerprint": { "type": "string" }
                    }
                },
                "ApplicationSkillCatalogEntry": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "invocation_token", "name", "description", "source", "run_mode", "trust", "available"],
                    "properties": {
                        "id": { "type": "string" },
                        "invocation_token": { "type": "string" },
                        "name": { "type": "string" },
                        "description": { "type": "string" },
                        "source": { "type": "string" },
                        "run_mode": { "type": "string" },
                        "trust": { "type": "string" },
                        "available": { "type": "boolean" },
                        "unavailable_reason": { "type": ["string", "null"] },
                        "binding": { "oneOf": [{ "$ref": "#/components/schemas/ApplicationSkillBinding" }, { "type": "null" }] }
                    }
                },
                "ApplicationAgentBinding": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["profile_id", "snapshot_id"],
                    "properties": {
                        "profile_id": { "type": "string" },
                        "snapshot_id": { "type": "string" }
                    }
                },
                "ApplicationAgentCatalogEntry": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "invocation_token", "description", "source", "kind", "trust", "enabled", "user_invocable", "available"],
                    "properties": {
                        "id": { "type": "string" },
                        "invocation_token": { "type": "string" },
                        "description": { "type": "string" },
                        "source": { "type": "string" },
                        "kind": { "type": "string" },
                        "trust": { "type": "string" },
                        "enabled": { "type": "boolean" },
                        "user_invocable": { "type": "boolean" },
                        "available": { "type": "boolean" },
                        "unavailable_reason": { "type": ["string", "null"] },
                        "snapshot_id": { "type": ["string", "null"] },
                        "binding": { "oneOf": [{ "$ref": "#/components/schemas/ApplicationAgentBinding" }, { "type": "null" }] }
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
                        "foreground_owner": {
                            "oneOf": [
                                { "$ref": "#/components/schemas/ForegroundRunOwner" },
                                { "type": "null" }
                            ]
                        },
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
                    "required": ["id", "session_id", "status", "permission_mode", "prompt_preview", "pending_approval_call_ids", "stream_sequence"],
                    "properties": {
                        "id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "status": { "$ref": "#/components/schemas/RunStatus" },
                        "permission_mode": { "$ref": "#/components/schemas/PermissionMode" },
                        "reasoning_effort": { "oneOf": [{ "$ref": "#/components/schemas/ReasoningEffort" }, { "type": "null" }] },
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
                    "enum": ["approve", "approve_for_session", "deny"]
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
