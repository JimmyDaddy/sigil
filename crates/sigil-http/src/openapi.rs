use serde_json::{Map, Value, json};

use crate::{
    HTTP_CONVERSATION_QUEUE_SCHEMA_VERSION, HTTP_MAX_CONVERSATION_QUEUE_ITEMS,
    HTTP_SERVER_INFO_SCHEMA_VERSION, protocol::HTTP_PROTOCOL_VERSION,
};

/// OpenAPI version emitted for the MVP desktop/app-server command surface.
pub const HTTP_OPENAPI_VERSION: &str = "3.1.0";

/// Returns the MVP OpenAPI description for the local HTTP command surface.
///
/// The document intentionally covers only routes implemented by this crate.
#[must_use]
pub fn http_openapi_document() -> Value {
    let mut document = json!({
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
            "/settings/provider-connections": {
                "get": {
                    "summary": "Read secret-free provider connection settings",
                    "description": "Returns connection identity, readiness, credential source classification, and the compound saved default. Credential values, stored-credential identifiers, raw private endpoints, config paths, and provider-private JSON are excluded.",
                    "responses": {
                        "200": { "description": "Native-owner provider connection inventory", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ProviderConnectionInventory" } } } },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                },
                "post": {
                    "summary": "Validate and atomically save one provider connection",
                    "description": "Validates the exact provider/model route, stores a supplied credential through the configured secure credential backend, and publishes the connection plus saved default as one recoverable operation.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/ProviderSetupSaveRequest" }
                            }
                        }
                    },
                    "responses": {
                        "201": { "description": "Saved provider connection and refreshed secret-free inventory", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ProviderSetupSaveResult" } } } },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "422": { "$ref": "#/components/responses/BadRequest" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                }
            },
            "/settings/provider-connections/catalog": {
                "post": {
                    "summary": "Load an exact connection-scoped provider model catalog",
                    "description": "Uses the selected template, endpoint, authentication source, and process-staged credential without publishing configuration. Catalog cache entries are reused when valid.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/ProviderSetupCatalogRequest" }
                            }
                        }
                    },
                    "responses": {
                        "200": { "description": "Connection-scoped model catalog", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ProviderSetupCatalog" } } } },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "422": { "$ref": "#/components/responses/BadRequest" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                }
            },
            "/settings/provider-connections/default-model": {
                "put": {
                    "summary": "Set the shared default model route",
                    "description": "Atomically selects one already configured exact connection/model route for future sessions. Existing durable sessions remain unchanged.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/ProviderDefaultModelSaveRequest" }
                            }
                        }
                    },
                    "responses": {
                        "200": { "description": "Saved exact default route and refreshed inventory", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ProviderDefaultModelSaveResult" } } } },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "422": { "$ref": "#/components/responses/BadRequest" },
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
                                "enum": ["ready", "oversized", "scan_budget_exceeded", "invalid"]
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
                    "summary": "Quarantine one exact unavailable local session source",
                    "description": "Revalidates the non-ready source metadata under maintenance and writer leases, then moves it into the local quarantine directory without exposing a filesystem path.",
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
                    "summary": "Permanently delete one exact unavailable local session source",
                    "description": "Revalidates the non-ready source fingerprint under maintenance and writer leases, then permanently removes the regular file after native-shell confirmation.",
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
            "/sessions/{session_id}/display": {
                "get": {
                    "summary": "Read one canonical durable conversation display page",
                    "description": "Returns stable identity/order projection from scope-checked append-only session truth. Durable sequences use decimal strings; raw durable scope, paths, checksums, credentials and tool arguments are excluded.",
                    "parameters": [
                        { "$ref": "#/components/parameters/SessionId" },
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
                            "description": "Opaque backwards cursor bound to one fixed durable frontier",
                            "schema": { "type": "string", "minLength": 1, "maxLength": 4096, "pattern": "^[A-Za-z0-9_-]+$" }
                        }
                    ],
                    "responses": {
                        "200": {
                            "description": "Canonical conversation display page",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ConversationDisplayPage" } } }
                        },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "409": { "$ref": "#/components/responses/Conflict" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                }
            },
            "/sessions/{session_id}/tool-artifacts/read": {
                "post": {
                    "summary": "Read one bounded typed tool artifact page",
                    "description": "Resolves an opaque artifact reference only in the addressed logical session, validates the immutable content hash, and returns a fixed-size byte, line, or literal-search page. Physical paths are never exposed.",
                    "parameters": [{ "$ref": "#/components/parameters/SessionId" }],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/ToolArtifactReadRequest" }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Bounded integrity-checked artifact page",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ToolArtifactPage" } } }
                        },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "409": { "$ref": "#/components/responses/Conflict" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                }
            },
            "/sessions/{session_id}/queue": {
                "get": {
                    "summary": "Read the durable follow-up queue",
                    "description": "Returns a bounded, secret-free queue view. Prompt hashes and process-local exact prompt material are excluded; generation is opaque and must be echoed unchanged.",
                    "parameters": [{ "$ref": "#/components/parameters/SessionId" }],
                    "responses": {
                        "200": {
                            "description": "Current bounded queue projection",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ConversationQueueView" } } }
                        },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "409": { "$ref": "#/components/responses/Conflict" },
                        "500": { "$ref": "#/components/responses/InternalError" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                },
                "post": {
                    "summary": "Apply one exact follow-up queue command",
                    "description": "Routes one idempotent enqueue, edit, remove, reorder, pause, resume, or owner-bound interrupt-and-run-next command under the opaque queue generation CAS guard.",
                    "parameters": [{ "$ref": "#/components/parameters/SessionId" }],
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ConversationQueueCommand" } } }
                    },
                    "responses": {
                        "200": {
                            "description": "Durable queue command receipt without exact prompt material",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ConversationQueueCommandReceipt" } } }
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
            "/sessions/{session_id}/recovery": {
                "get": {
                    "summary": "Read durable checkpoint and conversation-fork choices",
                    "description": "Projects exact digest-bound recovery choices without mutating files or session truth.",
                    "parameters": [{ "$ref": "#/components/parameters/SessionId" }],
                    "responses": {
                        "200": { "description": "Current durable recovery projection", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ConversationRecoveryView" } } } },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                }
            },
            "/sessions/{session_id}/recovery/checkpoint-preview": {
                "post": {
                    "summary": "Preview one exact controlled-file checkpoint restore",
                    "description": "Revalidates checkpoint digest, current file hashes, restorable snapshots, and bounded reverse diffs. No mutation is applied.",
                    "parameters": [{ "$ref": "#/components/parameters/SessionId" }],
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/CheckpointRestoreRequest" } } } },
                    "responses": {
                        "200": { "description": "Fresh restore review", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/CheckpointRestoreReview" } } } },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "409": { "$ref": "#/components/responses/Conflict" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                }
            },
            "/sessions/{session_id}/recovery/compaction-preview": {
                "post": {
                    "summary": "Preview one exact portable context compaction",
                    "description": "Builds a local-only fold, continuity and recoverable tool-output plan without contacting the provider, activating a compaction lifecycle or changing the visible projection. The returned process-local prepared preview can be kept unchanged, used for standalone tool-output cleanup, or explicitly advanced through prepare_compaction to one billed semantic-summary attempt. A ready semantic preview must still be explicitly applied before it becomes stale.",
                    "parameters": [{ "$ref": "#/components/parameters/SessionId" }],
                    "responses": {
                        "200": { "description": "Fresh compaction review", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/CompactionReview" } } } },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "409": { "$ref": "#/components/responses/Conflict" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                }
            },
            "/sessions/{session_id}/recovery/commands": {
                "post": {
                    "summary": "Apply one exact compaction, checkpoint restore, or conversation fork",
                    "description": "Routes one idempotent exactly-bound recovery command under durable session mutation exclusion. Restore affects only controlled durable file mutations; shell, network, remote, manual, and external side effects are not undone.",
                    "parameters": [{ "$ref": "#/components/parameters/SessionId" }],
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ConversationRecoveryCommand" } } } },
                    "responses": {
                        "200": { "description": "Durable recovery command receipt", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ConversationRecoveryCommandReceipt" } } } },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "409": { "$ref": "#/components/responses/Conflict" },
                        "500": { "$ref": "#/components/responses/InternalError" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
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
                        "409": { "$ref": "#/components/responses/RunAdmissionConflict" },
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
            "/sessions/{session_id}/intents": {
                "get": {
                    "summary": "Read the adapter-neutral durable Intent Stack",
                    "description": "Returns the same bounded projection consumed by TUI and automation. Raw patches, absolute paths, file content, policy and mutation authority are excluded.",
                    "parameters": [{ "$ref": "#/components/parameters/SessionId" }],
                    "responses": {
                        "200": {
                            "description": "Current durable Intent Stack or explicit history-unavailable state",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/IntentStackState" } } }
                        },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                }
            },
            "/sessions/{session_id}/intents/drop-preview": {
                "post": {
                    "summary": "Preview one exact leaf Intent Drop",
                    "description": "Rebuilds a digest-bound preview under durable mutation exclusion without applying file changes.",
                    "parameters": [{ "$ref": "#/components/parameters/SessionId" }],
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/IntentDropPreviewRequest" } } }
                    },
                    "responses": {
                        "200": {
                            "description": "Fresh exact Intent Drop preview",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/IntentOperationPreview" } } }
                        },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "409": { "$ref": "#/components/responses/Conflict" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                }
            },
            "/sessions/{session_id}/intents/drop": {
                "post": {
                    "summary": "Execute one exact confirmed Intent Drop",
                    "description": "Routes an idempotent digest-bound Drop command. The host reconstructs current permission, trust and confirmation authority; clients cannot submit paths, patches, file hashes or authority.",
                    "parameters": [{ "$ref": "#/components/parameters/SessionId" }],
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/IntentDropCommand" } } }
                    },
                    "responses": {
                        "200": {
                            "description": "Durable terminal Intent Drop receipt",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/IntentDropCommandReceipt" } } }
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
            "/sessions/{session_id}/task-integration/review": {
                "get": {
                    "summary": "Read one exact current Task integration review",
                    "description": "Returns the digest-verified aggregate diff and bounded lane provenance without private refs, worktree paths, object ids, artifact refs, or promotion authority.",
                    "parameters": [{ "$ref": "#/components/parameters/SessionId" }],
                    "responses": {
                        "200": {
                            "description": "Current exact Task integration review",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/TaskIntegrationReviewView" } } }
                        },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "409": { "$ref": "#/components/responses/Conflict" },
                        "500": { "$ref": "#/components/responses/InternalError" },
                        "503": { "$ref": "#/components/responses/Unavailable" }
                    }
                }
            },
            "/sessions/{session_id}/task-integration/accept": {
                "post": {
                    "summary": "Accept one exact current Task integration review",
                    "description": "Revalidates the stale-safe review identity, performs the content-bound promotion, and runs authoritative parent verification under durable session mutation exclusion.",
                    "parameters": [{ "$ref": "#/components/parameters/SessionId" }],
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/TaskIntegrationAcceptanceCommand" } } }
                    },
                    "responses": {
                        "200": {
                            "description": "Durable Task integration acceptance receipt",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/TaskIntegrationAcceptanceCommandReceipt" } } }
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
            "/runs/{run_id}/terminal-cancel": {
                "post": {
                    "summary": "Cancel one exact persistent terminal task",
                    "parameters": [{ "$ref": "#/components/parameters/RunId" }],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/TerminalTaskCancelCommand" }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Generation-bound terminal cancellation receipt",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/TerminalTaskCancelCommandReceipt" }
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
            "/runs/{run_id}/task-pause": {
                "post": {
                    "summary": "Pause one exact durable Task plan",
                    "parameters": [{ "$ref": "#/components/parameters/RunId" }],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/TaskPauseCommand" }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Task-pause command receipt",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/TaskPauseCommandReceipt" }
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
            "/sessions/{id}/plan-decision": {
                "post": {
                    "summary": "Apply one exact typed plan decision (Run, Save, Revise, Reject)",
                    "parameters": [{ "$ref": "#/components/parameters/SessionId" }],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/PlanDecisionCommand" }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Plan decision command receipt",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/PlanDecisionCommandReceipt" }
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
            "/sessions/{id}/plans/{plan_id}": {
                "get": {
                    "summary": "Read one exact immutable complete plan-review detail",
                    "parameters": [
                        { "$ref": "#/components/parameters/SessionId" },
                        { "name": "plan_id", "in": "path", "required": true, "schema": { "type": "string", "minLength": 1, "maxLength": 128 } },
                        { "name": "expected_plan_hash", "in": "query", "required": true, "schema": { "type": "string", "pattern": "^sha256:[0-9a-f]{64}$" } }
                    ],
                    "responses": {
                        "200": {
                            "description": "Exact complete plan detail",
                            "headers": { "ETag": { "schema": { "type": "string" } } },
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/PlanReviewDetail" } } }
                        },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "409": { "$ref": "#/components/responses/Conflict" },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/sessions/{id}/user-input/{request_id}": {
                "get": {
                    "summary": "Read one exact immutable durable user-input request",
                    "parameters": [
                        { "$ref": "#/components/parameters/SessionId" },
                        { "name": "request_id", "in": "path", "required": true, "schema": { "type": "string", "minLength": 1, "maxLength": 512 } },
                        { "name": "generation", "in": "query", "required": true, "schema": { "type": "integer", "format": "uint32", "minimum": 1 } },
                        { "name": "expected_request_hash", "in": "query", "required": true, "schema": { "$ref": "#/components/schemas/Sha256" } }
                    ],
                    "responses": {
                        "200": {
                            "description": "Exact public user-input request",
                            "headers": { "ETag": { "schema": { "type": "string" } } },
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/UserInputRequest" } } }
                        },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "409": { "$ref": "#/components/responses/Conflict" },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/sessions/{id}/user-input/{request_id}/decision": {
                "post": {
                    "summary": "Apply one exact durable user-input decision",
                    "parameters": [
                        { "$ref": "#/components/parameters/SessionId" },
                        { "name": "request_id", "in": "path", "required": true, "schema": { "type": "string", "minLength": 1, "maxLength": 512 } }
                    ],
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/UserInputDecisionCommand" } } } },
                    "responses": {
                        "200": { "description": "User-input decision receipt", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/UserInputDecisionCommandReceipt" } } } },
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
                                    "schema": { "$ref": "#/components/schemas/ProtocolEvent" }
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
                "RunAdmissionConflict": { "description": "The session route requires recovery or another interactive controller owns the session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/RunAdmissionErrorResponse" } } } },
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
                    "required": ["session_catalog", "durable_session_reopen", "bounded_transcript_replay", "canonical_conversation_display", "typed_tool_artifact_retrieval", "conversation_recovery", "durable_event_replay", "live_events", "approval", "durable_user_input", "cancellation", "task_pause", "terminal_task_cancel", "verification", "task_integration", "intent_stack", "run_context", "agent_activity", "support_diagnostics", "provider_connections", "provider_setup"],
                    "properties": {
                        "session_catalog": { "type": "boolean" },
                        "durable_session_reopen": { "type": "boolean" },
                        "bounded_transcript_replay": { "type": "boolean" },
                        "canonical_conversation_display": { "type": "boolean" },
                        "typed_tool_artifact_retrieval": { "type": "boolean" },
                        "conversation_recovery": { "type": "boolean" },
                        "durable_event_replay": { "type": "boolean" },
                        "live_events": { "type": "boolean" },
                        "approval": { "type": "boolean" },
                        "durable_user_input": { "type": "boolean" },
                        "cancellation": { "type": "boolean" },
                        "task_pause": { "type": "boolean" },
                        "terminal_task_cancel": { "type": "boolean" },
                        "verification": { "type": "boolean" },
                        "task_integration": { "type": "boolean" },
                        "intent_stack": { "type": "boolean" },
                        "run_context": { "type": "boolean" },
                        "agent_activity": { "type": "boolean" },
                        "support_diagnostics": { "type": "boolean" },
                        "provider_connections": { "type": "boolean" },
                        "provider_setup": { "type": "boolean" }
                    }
                },
                "ProviderConfigMode": {
                    "type": "string",
                    "enum": ["v2", "invalid"]
                },
                "ProviderModelRef": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["connection_id", "model_id"],
                    "properties": {
                        "connection_id": { "type": "string" },
                        "model_id": { "type": "string" }
                    }
                },
                "ProviderCredentialSource": {
                    "type": "string",
                    "enum": ["environment", "stored", "none"]
                },
                "ProviderConnectionReadiness": {
                    "type": "string",
                    "enum": ["ready", "needs_credential", "credential_unavailable", "needs_model", "unverified", "invalid"]
                },
                "ProviderConnectionIssue": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["code", "message"],
                    "properties": {
                        "code": { "type": "string" },
                        "message": { "type": "string" }
                    }
                },
                "ProviderConnectionEntry": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "label", "provider_label", "protocol_label", "endpoint_display", "credential_source", "readiness", "model_context_windows"],
                    "properties": {
                        "id": { "type": "string" },
                        "label": { "type": "string" },
                        "provider_label": { "type": "string" },
                        "protocol_label": { "type": "string" },
                        "endpoint_display": { "type": "string" },
                        "credential_source": { "$ref": "#/components/schemas/ProviderCredentialSource" },
                        "readiness": { "$ref": "#/components/schemas/ProviderConnectionReadiness" },
                        "model_context_windows": {
                            "type": "object",
                            "additionalProperties": { "type": "integer", "format": "uint32", "minimum": 1 }
                        },
                        "default_model": {
                            "anyOf": [
                                { "$ref": "#/components/schemas/ProviderModelRef" },
                                { "type": "null" }
                            ]
                        },
                        "issue": {
                            "anyOf": [
                                { "$ref": "#/components/schemas/ProviderConnectionIssue" },
                                { "type": "null" }
                            ]
                        }
                    }
                },
                "ProviderConnectionInventory": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["config_mode", "connections", "issues"],
                    "properties": {
                        "config_mode": { "$ref": "#/components/schemas/ProviderConfigMode" },
                        "default_model": {
                            "anyOf": [
                                { "$ref": "#/components/schemas/ProviderModelRef" },
                                { "type": "null" }
                            ]
                        },
                        "connections": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/ProviderConnectionEntry" }
                        },
                        "issues": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/ProviderConnectionIssue" }
                        }
                    }
                },
                "ProviderSetupTemplate": {
                    "type": "string",
                    "enum": ["deep_seek", "open_ai", "anthropic", "gemini", "open_ai_compatible"]
                },
                "ProviderSetupCredentialSource": {
                    "type": "string",
                    "enum": ["environment", "secure_store", "none"]
                },
                "ProviderSetupProtocol": {
                    "type": "string",
                    "enum": ["responses", "chat_completions"]
                },
                "ProviderSetupCatalogRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["template", "credential_source"],
                    "properties": {
                        "template": { "$ref": "#/components/schemas/ProviderSetupTemplate" },
                        "protocol": {
                            "anyOf": [
                                { "$ref": "#/components/schemas/ProviderSetupProtocol" },
                                { "type": "null" }
                            ]
                        },
                        "endpoint": { "type": ["string", "null"], "maxLength": 2048 },
                        "credential_source": { "$ref": "#/components/schemas/ProviderSetupCredentialSource" },
                        "api_key": { "type": ["string", "null"], "format": "password", "maxLength": 16384, "writeOnly": true },
                        "replace_invalid_config": { "type": "boolean", "default": false }
                    }
                },
                "ProviderSetupModel": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["model_id", "display_name", "availability", "recommended", "provenance"],
                    "properties": {
                        "model_id": { "type": "string" },
                        "display_name": { "type": "string" },
                        "availability": { "type": "string", "enum": ["available", "unverified", "configured_unavailable"] },
                        "recommended": { "type": "boolean" },
                        "provenance": { "type": "string", "enum": ["remote", "cache", "bundled", "configured", "manual"] },
                        "context_window_tokens": { "type": ["integer", "null"], "format": "uint32", "minimum": 1 }
                    }
                },
                "ProviderSetupCatalog": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["connection_id", "provider_label", "state", "models", "manual_entry_allowed"],
                    "properties": {
                        "connection_id": { "type": "string" },
                        "provider_label": { "type": "string" },
                        "state": { "type": "string" },
                        "models": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/ProviderSetupModel" }
                        },
                        "suggested_model": { "type": ["string", "null"] },
                        "manual_entry_allowed": { "type": "boolean" }
                    }
                },
                "ProviderSetupSaveRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["template", "credential_source", "model_id"],
                    "properties": {
                        "template": { "$ref": "#/components/schemas/ProviderSetupTemplate" },
                        "protocol": {
                            "anyOf": [
                                { "$ref": "#/components/schemas/ProviderSetupProtocol" },
                                { "type": "null" }
                            ]
                        },
                        "endpoint": { "type": ["string", "null"], "maxLength": 2048 },
                        "credential_source": { "$ref": "#/components/schemas/ProviderSetupCredentialSource" },
                        "api_key": { "type": ["string", "null"], "format": "password", "maxLength": 16384, "writeOnly": true },
                        "model_id": { "type": "string" },
                        "context_window_tokens": { "type": ["integer", "null"], "format": "uint32", "minimum": 1 },
                        "label": { "type": ["string", "null"], "maxLength": 160 },
                        "replace_invalid_config": { "type": "boolean", "default": false }
                    }
                },
                "ProviderSetupSaveResult": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["default_model", "inventory", "save_warning"],
                    "properties": {
                        "default_model": { "$ref": "#/components/schemas/ProviderModelRef" },
                        "inventory": { "$ref": "#/components/schemas/ProviderConnectionInventory" },
                        "save_warning": { "type": "boolean" }
                    }
                },
                "ProviderDefaultModelSaveRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["model_ref"],
                    "properties": {
                        "model_ref": { "$ref": "#/components/schemas/ProviderModelRef" },
                        "context_window_tokens": { "type": ["integer", "null"], "format": "uint32", "minimum": 1 }
                    }
                },
                "ProviderDefaultModelSaveResult": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["default_model", "inventory", "save_warning"],
                    "properties": {
                        "default_model": { "$ref": "#/components/schemas/ProviderModelRef" },
                        "inventory": { "$ref": "#/components/schemas/ProviderConnectionInventory" },
                        "save_warning": { "type": "boolean" }
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
                        "label": { "type": ["string", "null"], "maxLength": 160 },
                        "recovery_binding": { "type": ["string", "null"], "minLength": 1, "maxLength": 128 }
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
                    "required": ["id", "run_ids", "durable_session_scope_id"],
                    "properties": {
                        "id": { "type": "string" },
                        "label": { "type": ["string", "null"] },
                        "run_ids": { "type": "array", "items": { "type": "string" } },
                        "durable_session_scope_id": { "type": "string" },
                        "foreground_run_id": { "type": ["string", "null"] },
                        "route_transition": { "oneOf": [{ "$ref": "#/components/schemas/SessionRouteTransitionView" }, { "type": "null" }] },
                        "route_recovery": { "oneOf": [{ "$ref": "#/components/schemas/SessionRouteRecoveryView" }, { "type": "null" }] }
                    }
                },
                "SessionRouteTransitionView": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["kind", "remote_context_reset"],
                    "properties": {
                        "kind": { "type": "string", "enum": ["exact", "rebound", "explicitly_confirmed"] },
                        "connection_id": { "type": ["string", "null"] },
                        "model_id": { "type": ["string", "null"] },
                        "remote_context_reset": { "type": "boolean" }
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
                    "required": ["durable_session_scope_id", "durable_frontier", "retained_terminal_runs", "recovery_actions"],
                    "properties": {
                        "durable_session_scope_id": { "type": "string" },
                        "durable_frontier": { "$ref": "#/components/schemas/DurableSessionFrontier" },
                        "foreground_owner": {
                            "anyOf": [
                                { "$ref": "#/components/schemas/ForegroundRunOwner" },
                                { "type": "null" }
                            ]
                        },
                        "retained_terminal_runs": {
                            "type": "array",
                            "maxItems": 16,
                            "items": { "$ref": "#/components/schemas/RunSnapshot" }
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
                "ConversationDisplayOrder": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["session_stream_sequence", "subindex"],
                    "properties": {
                        "session_stream_sequence": { "$ref": "#/components/schemas/DecimalSequence" },
                        "subindex": { "type": "integer", "format": "uint32", "minimum": 0 }
                    }
                },
                "ConversationDisplayItemKind": {
                    "type": "string",
                    "enum": ["user_message", "reasoning", "assistant_message", "tool", "approval", "checkpoint", "notice", "terminal"]
                },
                "ConversationDisplaySource": {
                    "type": "string",
                    "enum": ["durable_transcript", "durable_run_event", "live_transient"]
                },
                "ConversationDisplayStatus": {
                    "type": "string",
                    "enum": ["recorded", "requested", "waiting_for_approval", "approved", "denied", "completed", "succeeded", "failed", "cancelled", "interrupted", "blocked"]
                },
                "ConversationDisplaySkillReference": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "name"],
                    "properties": {
                        "id": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "name": { "type": "string", "minLength": 1, "maxLength": 512 }
                    }
                },
                "ConversationDisplayContent": {
                    "oneOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["type", "role", "image_attachment_count", "truncated", "original_content_bytes"],
                            "properties": {
                                "type": { "const": "message" },
                                "role": { "type": "string", "enum": ["user", "assistant"] },
                                "text": { "type": ["string", "null"], "maxLength": 65536 },
                                "skill": {
                                    "anyOf": [
                                        { "$ref": "#/components/schemas/ConversationDisplaySkillReference" },
                                        { "type": "null" }
                                    ]
                                },
                                "assistant_phase": { "type": ["string", "null"], "enum": ["tool_preamble", "progress", "final_answer", null] },
                                "image_attachment_count": { "type": "integer", "format": "uint64" },
                                "truncated": { "type": "boolean" },
                                "original_content_bytes": { "type": "integer", "format": "uint64" }
                            }
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["type", "text", "truncated", "original_content_bytes"],
                            "properties": {
                                "type": { "const": "reasoning" },
                                "text": { "type": "string", "maxLength": 65536 },
                                "truncated": { "type": "boolean" },
                                "original_content_bytes": { "type": "integer", "format": "uint64" }
                            }
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["type", "truncated", "original_content_bytes", "has_more", "preview_truncated"],
                            "properties": {
                                "type": { "const": "tool" },
                                "call_id": { "type": ["string", "null"], "maxLength": 512 },
                                "tool_name": { "type": ["string", "null"], "maxLength": 512 },
                                "output": { "type": ["string", "null"], "maxLength": 65536 },
                                "truncated": { "type": "boolean" },
                                "original_content_bytes": { "type": "integer", "format": "uint64" },
                                "artifact_ref": { "type": ["string", "null"], "pattern": "^ta1_[0-9a-fA-F]{32}$" },
                                "artifact_availability": { "type": ["string", "null"], "enum": ["available", "expired", "missing", "hash_mismatch", "policy_revoked", "unavailable", null] },
                                "observed_bytes": { "type": ["integer", "null"], "format": "uint64" },
                                "persisted_bytes": { "type": ["integer", "null"], "format": "uint64", "maximum": 16777216 },
                                "has_more": { "type": "boolean" },
                                "preview_truncated": { "type": "boolean" },
                                "truncation_reason": { "type": ["string", "null"], "enum": ["initial_cap", "batch_budget", "binary_only", "fallback", null] },
                                "capture_completeness": { "type": ["string", "null"], "maxLength": 256 }
                            }
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["type", "call_id", "tool_name"],
                            "properties": {
                                "type": { "const": "approval" },
                                "call_id": { "type": "string", "maxLength": 512 },
                                "tool_name": { "type": "string", "maxLength": 512 },
                                "decision": { "type": ["string", "null"], "enum": ["approved", "approved_for_session", "denied", null] }
                            }
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["type", "outcome"],
                            "properties": {
                                "type": { "const": "checkpoint" },
                                "outcome": { "type": "string", "enum": ["restored", "conflict"] },
                                "checkpoint_id": { "type": ["string", "null"], "maxLength": 512 },
                                "conflict_reason": { "type": ["string", "null"], "enum": ["workspace_mismatch", "current_hash_mismatch", "intent_state_conflict", "artifact_unavailable", "sensitive_snapshot", "unsupported_snapshot", "invalid_binding", null] }
                            }
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["type", "text", "truncated", "original_content_bytes"],
                            "properties": {
                                "type": { "const": "notice" },
                                "text": { "type": "string", "maxLength": 65536 },
                                "truncated": { "type": "boolean" },
                                "original_content_bytes": { "type": "integer", "format": "uint64" }
                            }
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["type", "summary_truncated"],
                            "properties": {
                                "type": { "const": "terminal" },
                                "final_message_id": { "type": ["string", "null"], "maxLength": 512 },
                                "safe_summary": { "type": ["string", "null"], "maxLength": 65536 },
                                "summary_truncated": { "type": "boolean" }
                            }
                        }
                    ]
                },
                "ConversationDisplayItem": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["schema_version", "display_id", "display_order", "source_event_id", "kind", "source", "status", "content"],
                    "properties": {
                        "schema_version": { "type": "integer", "const": 1 },
                        "display_id": { "type": "string", "maxLength": 512 },
                        "display_order": { "$ref": "#/components/schemas/ConversationDisplayOrder" },
                        "source_event_id": { "type": "string", "maxLength": 512 },
                        "kind": { "$ref": "#/components/schemas/ConversationDisplayItemKind" },
                        "source": { "$ref": "#/components/schemas/ConversationDisplaySource" },
                        "run_id": { "type": ["string", "null"], "maxLength": 512 },
                        "run_sequence": { "oneOf": [{ "$ref": "#/components/schemas/DecimalSequence" }, { "type": "null" }] },
                        "status": { "$ref": "#/components/schemas/ConversationDisplayStatus" },
                        "content": { "$ref": "#/components/schemas/ConversationDisplayContent" },
                        "reconciles": { "type": ["array", "null"], "maxItems": 16, "items": { "type": "string", "maxLength": 512 } }
                    }
                },
                "ConversationTerminalFrontier": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["run_id", "session_stream_sequence", "status"],
                    "properties": {
                        "run_id": { "type": "string", "maxLength": 512 },
                        "session_stream_sequence": { "$ref": "#/components/schemas/DecimalSequence" },
                        "status": { "$ref": "#/components/schemas/ConversationDisplayStatus" }
                    }
                },
                "ConversationDisplayGapKind": {
                    "type": "string",
                    "enum": ["retention", "replay"]
                },
                "ConversationDisplayGapFact": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["kind", "after_session_stream_sequence"],
                    "properties": {
                        "kind": { "$ref": "#/components/schemas/ConversationDisplayGapKind" },
                        "after_session_stream_sequence": { "$ref": "#/components/schemas/DecimalSequence" }
                    }
                },
                "ConversationLiveProvisionalAnchor": {
                    "type": "object",
                    "additionalProperties": false,
                    "description": "Process-local observation only; never a durable display order.",
                    "required": ["durable_frontier", "run_id", "run_sequence"],
                    "properties": {
                        "durable_frontier": { "$ref": "#/components/schemas/DecimalSequence" },
                        "run_id": { "type": "string", "maxLength": 512 },
                        "run_sequence": { "$ref": "#/components/schemas/DecimalSequence" }
                    }
                },
                "ConversationTaskPlanStep": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["step_id", "title", "role", "depends_on", "mode", "isolation"],
                    "properties": {
                        "step_id": { "type": "string", "maxLength": 512 },
                        "title": { "type": "string", "maxLength": 4096 },
                        "role": { "type": "string", "maxLength": 512 },
                        "depends_on": { "type": "array", "maxItems": 32, "items": { "type": "string", "maxLength": 512 } },
                        "mode": { "type": "string", "maxLength": 512 },
                        "isolation": { "type": "string", "maxLength": 512 },
                        "status": { "type": "string", "maxLength": 512 }
                    }
                },
                "ConversationTaskLane": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["lane_id", "status", "conflicts"],
                    "properties": {
                        "lane_id": { "type": "string", "maxLength": 512 },
                        "plan_id": { "type": "string", "maxLength": 512 },
                        "status": { "type": "string", "maxLength": 512 },
                        "conflicts": { "type": "array", "maxItems": 32, "items": { "type": "string", "maxLength": 512 } }
                    }
                },
                "ConversationTaskControl": {
                    "type": "object",
                    "additionalProperties": false,
                    "description": "Bounded durable Task controls without objective, prompt, transcript, path, ref, or mutation authority.",
                    "required": ["schema_version", "task_id", "phase", "status", "steps", "steps_truncated", "active_children", "completed_children", "failed_children", "lanes", "lanes_truncated", "can_continue"],
                    "properties": {
                        "schema_version": { "type": "integer", "const": 1 },
                        "task_id": { "type": "string", "maxLength": 512 },
                        "phase": { "$ref": "#/components/schemas/PublicTaskPhase" },
                        "status": { "type": "string", "maxLength": 512 },
                        "plan_version": { "type": "integer", "format": "uint32" },
                        "plan_status": { "type": "string", "maxLength": 512 },
                        "steps": { "type": "array", "maxItems": 128, "items": { "$ref": "#/components/schemas/ConversationTaskPlanStep" } },
                        "steps_truncated": { "type": "boolean" },
                        "active_children": { "type": "integer", "format": "uint32" },
                        "completed_children": { "type": "integer", "format": "uint32" },
                        "failed_children": { "type": "integer", "format": "uint32" },
                        "lanes": { "type": "array", "maxItems": 128, "items": { "$ref": "#/components/schemas/ConversationTaskLane" } },
                        "lanes_truncated": { "type": "boolean" },
                        "can_continue": { "type": "boolean" }
                    }
                },
                "ConversationDisplayPage": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["schema_version", "request_scope", "through_session_stream_sequence", "total_items", "items", "has_more", "gap_facts"],
                    "properties": {
                        "schema_version": { "type": "integer", "const": 1 },
                        "request_scope": { "type": "string", "maxLength": 512 },
                        "through_session_stream_sequence": { "$ref": "#/components/schemas/DecimalSequence" },
                        "terminal_frontier": { "oneOf": [{ "$ref": "#/components/schemas/ConversationTerminalFrontier" }, { "type": "null" }] },
                        "total_items": { "$ref": "#/components/schemas/DecimalSequence" },
                        "items": { "type": "array", "maxItems": 100, "items": { "$ref": "#/components/schemas/ConversationDisplayItem" } },
                        "next_cursor": { "type": ["string", "null"], "maxLength": 4096, "pattern": "^[A-Za-z0-9_-]+$" },
                        "has_more": { "type": "boolean" },
                        "gap_facts": { "type": "array", "maxItems": 8, "items": { "$ref": "#/components/schemas/ConversationDisplayGapFact" } },
                        "live_provisional_anchor": { "oneOf": [{ "$ref": "#/components/schemas/ConversationLiveProvisionalAnchor" }, { "type": "null" }] },
                        "task_control": { "oneOf": [{ "$ref": "#/components/schemas/ConversationTaskControl" }, { "type": "null" }] },
                        "plan_review": { "oneOf": [{ "$ref": "#/components/schemas/PlanReview" }, { "type": "null" }] },
                        "user_input": { "oneOf": [{ "$ref": "#/components/schemas/UserInputRequest" }, { "type": "null" }] }
                    }
                },
                "PlanReview": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["plan_id", "status", "summary_truncated", "allowed_actions", "source", "stale"],
                    "properties": {
                        "plan_id": { "type": "string", "maxLength": 128 },
                        "plan_hash": { "type": ["string", "null"], "maxLength": 128 },
                        "status": { "type": "string", "enum": ["started", "waiting_for_input", "finalizing", "draft_ready", "completed_without_draft", "failed", "interrupted", "cancelled"] },
                        "summary": { "type": ["string", "null"], "maxLength": 512 },
                        "summary_truncated": { "type": "boolean" },
                        "step_count": { "type": ["integer", "null"], "minimum": 0 },
                        "target_path_count": { "type": ["integer", "null"], "minimum": 0 },
                        "suggested_check_count": { "type": ["integer", "null"], "minimum": 0 },
                        "risk": { "type": ["string", "null"], "maxLength": 512 },
                        "allowed_actions": { "type": "array", "items": { "type": "string", "enum": ["run", "save", "revise", "reject"] } },
                        "source": { "type": "string", "enum": ["explicit_plan_command", "automatic_conversation_route"] },
                        "stale": { "type": "boolean" },
                        "revision": {
                            "oneOf": [
                                {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "required": ["request_id", "status"],
                                    "properties": {
                                        "request_id": { "type": "string" },
                                        "attempt_id": { "type": ["string", "null"] },
                                        "attempt_ordinal": { "type": ["integer", "null"], "minimum": 1 },
                                        "status": { "type": "string", "enum": ["awaiting_guidance", "queued", "researching", "waiting_for_input", "finalizing", "failed", "cancelled", "succeeded"] },
                                        "terminal_reason": { "type": ["string", "null"] }
                                    }
                                },
                                { "type": "null" }
                            ]
                        }
                    }
                },
                "PlanSuggestedCheck": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["check_spec_id", "command", "effect"],
                    "properties": {
                        "check_spec_id": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "command": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["command", "args"],
                            "properties": {
                                "command": { "type": "string", "minLength": 1, "maxLength": 4096 },
                                "args": { "type": "array", "maxItems": 256, "items": { "type": "string", "maxLength": 4096 } },
                                "cwd": { "type": ["string", "null"], "maxLength": 4096 }
                            }
                        },
                        "effect": { "type": "string", "enum": ["read_only", "workspace_write", "external_write", "network", "unknown"] },
                        "source_line": { "type": ["string", "null"], "maxLength": 4096 }
                    }
                },
                "PlanReviewStepDetail": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["step_id", "title", "depends_on", "target_paths", "suggested_checks", "notes"],
                    "properties": {
                        "step_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                        "title": { "type": "string", "minLength": 1, "maxLength": 4096 },
                        "display_name": { "type": ["string", "null"], "maxLength": 4096 },
                        "detail": { "type": ["string", "null"], "maxLength": 16384 },
                        "role": { "oneOf": [{ "type": "string", "enum": ["planner", "executor", "subagent_read", "subagent_write"] }, { "type": "null" }] },
                        "depends_on": { "type": "array", "maxItems": 256, "items": { "type": "string", "maxLength": 128 } },
                        "mode": { "oneOf": [{ "type": "string", "enum": ["read", "write", "review", "verify"] }, { "type": "null" }] },
                        "isolation": { "oneOf": [{ "type": "string", "enum": ["shared_read_only", "sequential_workspace_write", "changeset_only", "worktree"] }, { "type": "null" }] },
                        "target_paths": { "type": "array", "maxItems": 512, "items": { "type": "string", "maxLength": 4096 } },
                        "suggested_checks": { "type": "array", "maxItems": 256, "items": { "$ref": "#/components/schemas/PlanSuggestedCheck" } },
                        "risk": { "type": ["string", "null"], "maxLength": 4096 },
                        "notes": { "type": "array", "maxItems": 256, "items": { "type": "string", "maxLength": 4096 } }
                    }
                },
                "PlanLineage": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["source", "created_at_ms"],
                    "properties": {
                        "source": { "type": "object" },
                        "plan_review_id": { "type": ["string", "null"], "maxLength": 128 },
                        "attempt_id": { "type": ["string", "null"], "maxLength": 128 },
                        "created_at_ms": { "type": "integer", "format": "uint64" }
                    }
                },
                "PlanReviewDetail": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["plan_id", "plan_hash", "source", "summary", "steps", "target_paths", "suggested_checks", "notes", "lineage"],
                    "properties": {
                        "plan_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                        "plan_hash": { "type": "string", "pattern": "^sha256:[0-9a-f]{64}$" },
                        "workspace_snapshot_id": { "type": ["string", "null"], "maxLength": 512 },
                        "source": { "type": "string", "enum": ["explicit_plan_command", "automatic_conversation_route"] },
                        "summary": { "type": "string", "minLength": 1, "maxLength": 2048 },
                        "steps": { "type": "array", "maxItems": 256, "items": { "$ref": "#/components/schemas/PlanReviewStepDetail" } },
                        "target_paths": { "type": "array", "maxItems": 512, "items": { "type": "string", "maxLength": 4096 } },
                        "suggested_checks": { "type": "array", "maxItems": 256, "items": { "$ref": "#/components/schemas/PlanSuggestedCheck" } },
                        "risk": { "type": ["string", "null"], "maxLength": 4096 },
                        "notes": { "type": "array", "maxItems": 256, "items": { "type": "string", "maxLength": 4096 } },
                        "lineage": { "$ref": "#/components/schemas/PlanLineage" },
                        "legacy_markdown": { "type": ["string", "null"], "maxLength": 65536 }
                    }
                },
                "PlanDecisionCommand": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["protocol_version", "command_id", "client_id", "session_id", "payload"],
                    "properties": {
                        "protocol_version": { "type": "integer", "const": 2 },
                        "command_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                        "client_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                        "session_id": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "expected_stream_sequence": { "type": ["string", "null"] },
                        "correlation_id": { "type": ["string", "null"], "maxLength": 128 },
                        "payload": { "$ref": "#/components/schemas/PlanDecisionRequest" }
                    }
                },
                "PlanDecisionRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["plan_id", "expected_plan_hash", "action"],
                    "properties": {
                        "plan_id": { "type": "string", "maxLength": 128 },
                        "expected_plan_hash": { "type": "string", "maxLength": 128 },
                        "action": { "type": "string", "enum": ["run", "save", "revise", "reject"] },
                        "permission_grant": { "oneOf": [{ "type": "string", "enum": ["ask", "workspace_edits"] }, { "type": "null" }] }
                    }
                },
                "PlanDecisionCommandReceipt": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["command_id", "client_id", "session_id", "plan_id", "plan_hash", "action", "replayed"],
                    "properties": {
                        "command_id": { "type": "string" },
                        "client_id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "plan_id": { "type": "string" },
                        "plan_hash": { "type": "string" },
                        "action": { "type": "string", "enum": ["run", "save", "revise", "reject"] },
                        "task_id": { "type": ["string", "null"] },
                        "revision_run_id": { "type": ["string", "null"] },
                        "user_input_request": { "oneOf": [{ "$ref": "#/components/schemas/UserInputRequest" }, { "type": "null" }] },
                        "replayed": { "type": "boolean" }
                    }
                },
                "Sha256": {
                    "type": "string",
                    "minLength": 71,
                    "maxLength": 71,
                    "pattern": "^sha256:[0-9a-fA-F]{64}$"
                },
                "UserInputIdentity": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["session_scope_id", "root_logical_run_id", "source_thread_id", "request_id", "generation", "source_binding_hash"],
                    "properties": {
                        "session_scope_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                        "root_logical_run_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                        "source_thread_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                        "request_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                        "generation": { "type": "integer", "format": "uint32", "minimum": 1 },
                        "source_binding_hash": { "$ref": "#/components/schemas/Sha256" }
                    }
                },
                "UserInputOption": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "label"],
                    "properties": {
                        "id": { "type": "string", "minLength": 1, "maxLength": 48 },
                        "label": { "type": "string", "minLength": 1, "maxLength": 80 },
                        "description": { "type": ["string", "null"], "maxLength": 240 }
                    }
                },
                "UserInputField": {
                    "oneOf": [
                        { "type": "object", "additionalProperties": false, "required": ["kind", "multiline", "max_chars"], "properties": { "kind": { "const": "text" }, "multiline": { "type": "boolean" }, "max_chars": { "type": "integer", "format": "uint32", "minimum": 1, "maximum": 4096 } } },
                        { "type": "object", "additionalProperties": false, "required": ["kind"], "properties": { "kind": { "const": "number" } } },
                        { "type": "object", "additionalProperties": false, "required": ["kind"], "properties": { "kind": { "const": "integer" } } },
                        { "type": "object", "additionalProperties": false, "required": ["kind"], "properties": { "kind": { "const": "boolean" } } },
                        { "type": "object", "additionalProperties": false, "required": ["kind", "options", "allow_other"], "properties": { "kind": { "const": "single_select" }, "options": { "type": "array", "minItems": 2, "maxItems": 12, "items": { "$ref": "#/components/schemas/UserInputOption" } }, "allow_other": { "type": "boolean" } } },
                        { "type": "object", "additionalProperties": false, "required": ["kind", "options", "max_selected"], "properties": { "kind": { "const": "multi_select" }, "options": { "type": "array", "minItems": 2, "maxItems": 12, "items": { "$ref": "#/components/schemas/UserInputOption" } }, "max_selected": { "type": "integer", "format": "uint32", "minimum": 1, "maximum": 12 } } }
                    ]
                },
                "UserInputQuestion": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "header", "question", "required", "field"],
                    "properties": {
                        "id": { "type": "string", "minLength": 1, "maxLength": 48 },
                        "header": { "type": "string", "minLength": 1, "maxLength": 32 },
                        "question": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "description": { "type": ["string", "null"], "maxLength": 512 },
                        "required": { "type": "boolean" },
                        "field": { "$ref": "#/components/schemas/UserInputField" }
                    }
                },
                "UserInputRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["identity", "request_hash", "source", "purpose", "prompt", "questions", "allowed_actions", "requested_at_unix_ms", "status"],
                    "properties": {
                        "identity": { "$ref": "#/components/schemas/UserInputIdentity" },
                        "request_hash": { "$ref": "#/components/schemas/Sha256" },
                        "source": { "oneOf": [
                            { "type": "string", "enum": ["agent"] },
                            { "type": "object", "additionalProperties": false, "required": ["plan_review_research"], "properties": { "plan_review_research": { "type": "object", "additionalProperties": false, "required": ["plan_review_id", "attempt_id"], "properties": { "plan_review_id": { "type": "string" }, "attempt_id": { "type": "string" } } } } },
                            { "type": "object", "additionalProperties": false, "required": ["plan_revision"], "properties": { "plan_revision": { "type": "object", "additionalProperties": false, "required": ["base_plan_id", "base_plan_hash"], "properties": { "base_plan_id": { "type": "string" }, "base_plan_hash": { "$ref": "#/components/schemas/Sha256" } } } } },
                            { "type": "object", "additionalProperties": false, "required": ["planner"], "properties": { "planner": { "type": "object", "additionalProperties": false, "required": ["task_id"], "properties": { "task_id": { "type": "string" } } } } },
                            { "type": "object", "additionalProperties": false, "required": ["mcp"], "properties": { "mcp": { "type": "object", "additionalProperties": false, "required": ["server_id", "call_id"], "properties": { "server_id": { "type": "string" }, "call_id": { "type": "string" } } } } }
                        ] },
                        "purpose": { "type": "string", "enum": ["clarification", "choice", "missing_constraint", "revision_guidance", "external_elicitation"] },
                        "prompt": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "questions": { "type": "array", "minItems": 1, "maxItems": 3, "items": { "$ref": "#/components/schemas/UserInputQuestion" } },
                        "allowed_actions": { "type": "array", "uniqueItems": true, "items": { "type": "string", "enum": ["submit", "decline", "cancel_run"] } },
                        "requested_at_unix_ms": { "type": "integer", "format": "uint64" },
                        "status": { "type": "string", "enum": ["requested", "decision_accepted", "continuation_claimed", "continuation_started", "resolved"] },
                        "answer_receipt": { "oneOf": [{ "$ref": "#/components/schemas/UserInputAnswerReceipt" }, { "type": "null" }] },
                        "resolution": { "oneOf": [
                            { "type": "string", "enum": ["consumed", "declined", "run_cancelled"] },
                            { "type": "object", "additionalProperties": false, "required": ["failed"], "properties": { "failed": { "type": "object", "additionalProperties": false, "required": ["failure_class", "retryable"], "properties": { "failure_class": { "type": "string", "maxLength": 128 }, "retryable": { "type": "boolean" } } } } },
                            { "type": "null" }
                        ] }
                    }
                },
                "UserInputAnswerReceipt": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["command_id", "decision"],
                    "properties": {
                        "command_id": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "decision": { "type": "string", "enum": ["submitted", "declined", "run_cancelled"] },
                        "answer_hash": { "oneOf": [{ "$ref": "#/components/schemas/Sha256" }, { "type": "null" }] },
                        "answered_question_ids": { "type": "array", "maxItems": 3, "items": { "type": "string" } }
                    }
                },
                "UserInputAnswerValue": {
                    "oneOf": [
                        { "type": "object", "additionalProperties": false, "required": ["kind", "value"], "properties": { "kind": { "const": "text" }, "value": { "type": "string", "maxLength": 4096 } } },
                        { "type": "object", "additionalProperties": false, "required": ["kind", "value"], "properties": { "kind": { "const": "number" }, "value": { "type": "string", "maxLength": 64 } } },
                        { "type": "object", "additionalProperties": false, "required": ["kind", "value"], "properties": { "kind": { "const": "integer" }, "value": { "type": "integer", "format": "int64" } } },
                        { "type": "object", "additionalProperties": false, "required": ["kind", "value"], "properties": { "kind": { "const": "boolean" }, "value": { "type": "boolean" } } },
                        { "type": "object", "additionalProperties": false, "required": ["kind"], "properties": { "kind": { "const": "single_select" }, "option_id": { "type": ["string", "null"], "maxLength": 48 }, "other": { "type": ["string", "null"], "maxLength": 512 } } },
                        { "type": "object", "additionalProperties": false, "required": ["kind", "option_ids"], "properties": { "kind": { "const": "multi_select" }, "option_ids": { "type": "array", "maxItems": 12, "items": { "type": "string", "maxLength": 48 } } } }
                    ]
                },
                "UserInputDecision": {
                    "oneOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["kind", "answers"],
                            "properties": {
                                "kind": { "const": "submitted" },
                                "answers": {
                                    "type": "array",
                                    "maxItems": 3,
                                    "items": {
                                        "type": "object",
                                        "additionalProperties": false,
                                        "required": ["question_id", "value"],
                                        "properties": {
                                            "question_id": { "type": "string" },
                                            "value": { "$ref": "#/components/schemas/UserInputAnswerValue" }
                                        }
                                    }
                                }
                            }
                        },
                        { "type": "object", "additionalProperties": false, "required": ["kind"], "properties": { "kind": { "const": "declined" } } },
                        { "type": "object", "additionalProperties": false, "required": ["kind"], "properties": { "kind": { "const": "run_cancelled" } } }
                    ]
                },
                "UserInputDecisionRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["generation", "expected_request_hash", "decision"],
                    "properties": {
                        "generation": { "type": "integer", "format": "uint32", "minimum": 1 },
                        "expected_request_hash": { "$ref": "#/components/schemas/Sha256" },
                        "decision": { "$ref": "#/components/schemas/UserInputDecision" },
                        "permission_mode": { "oneOf": [{ "$ref": "#/components/schemas/PermissionMode" }, { "type": "null" }] }
                    }
                },
                "UserInputDecisionCommand": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["protocol_version", "command_id", "client_id", "session_id", "payload"],
                    "properties": {
                        "protocol_version": { "type": "integer", "const": 2 },
                        "command_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                        "client_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                        "session_id": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "expected_stream_sequence": { "type": ["string", "null"] },
                        "correlation_id": { "type": ["string", "null"], "maxLength": 128 },
                        "payload": { "$ref": "#/components/schemas/UserInputDecisionRequest" }
                    }
                },
                "UserInputDecisionCommandReceipt": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["command_id", "client_id", "session_id", "request", "replayed"],
                    "properties": {
                        "command_id": { "type": "string" },
                        "client_id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "request": { "$ref": "#/components/schemas/UserInputRequest" },
                        "continuation_run_id": { "type": ["string", "null"] },
                        "replayed": { "type": "boolean" }
                    }
                },
                "ToolArtifactSelector": {
                    "oneOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["kind", "offset", "limit"],
                            "properties": {
                                "kind": { "type": "string", "const": "byte_slice" },
                                "offset": { "type": "integer", "format": "uint64", "minimum": 0, "maximum": 16777216 },
                                "limit": { "type": "integer", "format": "uint32", "minimum": 1, "maximum": 16384 }
                            }
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["kind", "start_line", "line_count"],
                            "properties": {
                                "kind": { "type": "string", "const": "line_page" },
                                "start_line": { "type": "integer", "format": "uint64", "minimum": 0, "maximum": 16777216 },
                                "line_count": { "type": "integer", "format": "uint32", "minimum": 1, "maximum": 200 }
                            }
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["kind", "query", "start_offset", "max_matches", "context_lines"],
                            "properties": {
                                "kind": { "type": "string", "const": "search_literal" },
                                "query": { "type": "string", "minLength": 1, "maxLength": 512 },
                                "start_offset": { "type": "integer", "format": "uint64", "minimum": 0, "maximum": 16777216 },
                                "max_matches": { "type": "integer", "format": "uint16", "minimum": 1, "maximum": 20 },
                                "context_lines": { "type": "integer", "format": "uint16", "minimum": 0, "maximum": 3 }
                            }
                        }
                    ]
                },
                "ToolArtifactReadRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["artifact_ref", "selector"],
                    "properties": {
                        "artifact_ref": { "type": "string", "pattern": "^ta1_[0-9a-fA-F]{32}$" },
                        "selector": { "$ref": "#/components/schemas/ToolArtifactSelector" }
                    }
                },
                "ToolArtifactPageEncoding": {
                    "type": "string",
                    "enum": ["utf8", "base64"]
                },
                "ToolArtifactPage": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["schema_version", "request_scope", "artifact_ref", "selector", "body", "body_encoding", "returned_bytes", "page_sha256", "artifact_sha256", "eof", "match_count"],
                    "properties": {
                        "schema_version": { "type": "integer", "const": 1 },
                        "request_scope": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "artifact_ref": { "type": "string", "pattern": "^ta1_[0-9a-fA-F]{32}$" },
                        "selector": { "$ref": "#/components/schemas/ToolArtifactSelector" },
                        "body": { "type": "string", "maxLength": 21848 },
                        "body_encoding": { "$ref": "#/components/schemas/ToolArtifactPageEncoding" },
                        "returned_bytes": { "type": "integer", "format": "uint32", "minimum": 0, "maximum": 16384 },
                        "page_sha256": { "type": "string", "pattern": "^sha256:[0-9a-f]{64}$" },
                        "artifact_sha256": { "type": "string", "pattern": "^sha256:[0-9a-f]{64}$" },
                        "eof": { "type": "boolean" },
                        "match_count": { "type": "integer", "format": "uint16", "minimum": 0, "maximum": 20 },
                        "next_selector": { "oneOf": [{ "$ref": "#/components/schemas/ToolArtifactSelector" }, { "type": "null" }] }
                    }
                },
                "ConversationQueueGeneration": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 512,
                    "pattern": "^[A-Za-z0-9._:-]+$",
                    "description": "Opaque queue CAS generation. Clients must echo it unchanged."
                },
                "ConversationQueueItemKind": {
                    "type": "string",
                    "enum": ["chat", "plan_prompt", "agent_mention", "agent_message", "unknown"]
                },
                "ConversationQueueItemStatus": {
                    "type": "string",
                    "enum": ["queued", "dispatching", "delivered", "rejected", "cancelled", "stale", "unknown"]
                },
                "ConversationQueuePromptMaterial": {
                    "type": "string",
                    "enum": ["persisted_safe", "available_process_local", "requires_reentry"]
                },
                "ConversationQueueBlockedReason": {
                    "type": "string",
                    "enum": ["queue_paused", "requires_reentry", "foreground_run_active", "waiting_for_terminal_frontier", "foreground_owner_lost", "permission_required", "conflict", "stale", "terminal", "unsupported_target", "material_unavailable"]
                },
                "ConversationQueueItem": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["entry_id", "order", "kind", "status", "prompt_preview", "prompt_preview_truncated", "prompt_material", "dispatchable"],
                    "properties": {
                        "entry_id": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "order": { "type": "integer", "format": "uint32", "minimum": 0, "maximum": HTTP_MAX_CONVERSATION_QUEUE_ITEMS - 1 },
                        "kind": { "$ref": "#/components/schemas/ConversationQueueItemKind" },
                        "status": { "$ref": "#/components/schemas/ConversationQueueItemStatus" },
                        "prompt_preview": { "type": "string", "maxLength": 240 },
                        "prompt_preview_truncated": { "type": "boolean" },
                        "prompt_material": { "$ref": "#/components/schemas/ConversationQueuePromptMaterial" },
                        "dispatchable": { "type": "boolean" },
                        "blocked_reason": { "oneOf": [{ "$ref": "#/components/schemas/ConversationQueueBlockedReason" }, { "type": "null" }] },
                        "created_at_ms": { "type": ["integer", "null"], "format": "uint64" },
                        "updated_at_ms": { "type": ["integer", "null"], "format": "uint64" }
                    }
                },
                "ConversationQueueView": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["schema_version", "session_id", "generation", "paused", "total_items", "items", "truncated"],
                    "properties": {
                        "schema_version": { "type": "integer", "const": HTTP_CONVERSATION_QUEUE_SCHEMA_VERSION },
                        "session_id": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "generation": { "$ref": "#/components/schemas/ConversationQueueGeneration" },
                        "paused": { "type": "boolean" },
                        "total_items": { "type": "integer", "format": "uint32", "minimum": 0 },
                        "items": { "type": "array", "maxItems": HTTP_MAX_CONVERSATION_QUEUE_ITEMS, "items": { "$ref": "#/components/schemas/ConversationQueueItem" } },
                        "truncated": { "type": "boolean" },
                        "next_dispatchable_entry_id": { "type": ["string", "null"], "minLength": 1, "maxLength": 512 }
                    }
                },
                "ConversationQueueEnqueueAction": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["action", "prompt", "kind"],
                    "properties": {
                        "action": { "type": "string", "const": "enqueue" },
                        "prompt": { "type": "string", "minLength": 1, "maxLength": 65536 },
                        "kind": { "$ref": "#/components/schemas/ConversationQueueItemKind" },
                        "reasoning_effort": { "oneOf": [{ "$ref": "#/components/schemas/ReasoningEffort" }, { "type": "null" }] }
                    }
                },
                "ConversationQueueEditAction": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["action", "entry_id", "prompt"],
                    "properties": {
                        "action": { "type": "string", "const": "edit" },
                        "entry_id": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "prompt": { "type": "string", "minLength": 1, "maxLength": 65536 },
                        "reasoning_effort": { "oneOf": [{ "$ref": "#/components/schemas/ReasoningEffort" }, { "type": "null" }] }
                    }
                },
                "ConversationQueueRemoveAction": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["action", "entry_id"],
                    "properties": {
                        "action": { "type": "string", "const": "remove" },
                        "entry_id": { "type": "string", "minLength": 1, "maxLength": 512 }
                    }
                },
                "ConversationQueueReorderAction": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["action", "entry_id"],
                    "properties": {
                        "action": { "type": "string", "const": "reorder" },
                        "entry_id": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "after_entry_id": { "type": ["string", "null"], "minLength": 1, "maxLength": 512 }
                    }
                },
                "ConversationQueuePauseAction": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["action"],
                    "properties": { "action": { "type": "string", "const": "pause" } }
                },
                "ConversationQueueResumeAction": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["action"],
                    "properties": { "action": { "type": "string", "const": "resume" } }
                },
                "ConversationQueueInterruptAndRunNextAction": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["action", "foreground_run_id", "foreground_owner_revision"],
                    "properties": {
                        "action": { "type": "string", "const": "interrupt_and_run_next" },
                        "foreground_run_id": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "foreground_owner_revision": { "type": "string", "minLength": 71, "maxLength": 71, "pattern": "^sha256:[0-9a-f]{64}$" }
                    }
                },
                "ConversationQueueCommandAction": {
                    "oneOf": [
                        { "$ref": "#/components/schemas/ConversationQueueEnqueueAction" },
                        { "$ref": "#/components/schemas/ConversationQueueEditAction" },
                        { "$ref": "#/components/schemas/ConversationQueueRemoveAction" },
                        { "$ref": "#/components/schemas/ConversationQueueReorderAction" },
                        { "$ref": "#/components/schemas/ConversationQueuePauseAction" },
                        { "$ref": "#/components/schemas/ConversationQueueResumeAction" },
                        { "$ref": "#/components/schemas/ConversationQueueInterruptAndRunNextAction" }
                    ]
                },
                "ConversationQueueCommandActionKind": {
                    "type": "string",
                    "enum": ["enqueue", "edit", "remove", "reorder", "pause", "resume", "interrupt_and_run_next"]
                },
                "ConversationQueueCommandRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["expected_generation", "action"],
                    "properties": {
                        "expected_generation": { "$ref": "#/components/schemas/ConversationQueueGeneration" },
                        "action": { "$ref": "#/components/schemas/ConversationQueueCommandAction" }
                    }
                },
                "ConversationQueueCommand": {
                    "allOf": [
                        { "$ref": "#/components/schemas/CommandEnvelopeBase" },
                        {
                            "type": "object",
                            "required": ["payload"],
                            "properties": {
                                "payload": { "$ref": "#/components/schemas/ConversationQueueCommandRequest" }
                            }
                        }
                    ]
                },
                "ConversationQueueCommandReceipt": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["command_id", "client_id", "session_id", "action", "expected_generation", "generation", "queue", "replayed"],
                    "properties": {
                        "command_id": { "type": "string" },
                        "client_id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "action": { "$ref": "#/components/schemas/ConversationQueueCommandActionKind" },
                        "expected_generation": { "$ref": "#/components/schemas/ConversationQueueGeneration" },
                        "generation": { "$ref": "#/components/schemas/ConversationQueueGeneration" },
                        "interrupt_owner": {
                            "oneOf": [
                                { "$ref": "#/components/schemas/ForegroundRunOwner" },
                                { "type": "null" }
                            ]
                        },
                        "queue": { "$ref": "#/components/schemas/ConversationQueueView" },
                        "correlation_id": { "type": ["string", "null"] },
                        "replayed": { "type": "boolean" }
                    }
                },
                "CheckpointRestoreKind": {
                    "type": "string",
                    "enum": ["restore_content", "remove_created_file"]
                },
                "CheckpointFileAvailability": {
                    "type": "string",
                    "enum": ["restorable", "sensitive", "unsupported", "unavailable"]
                },
                "CheckpointFileView": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["path", "restore_kind", "availability"],
                    "properties": {
                        "path": { "type": "string", "maxLength": 4096 },
                        "restore_kind": { "$ref": "#/components/schemas/CheckpointRestoreKind" },
                        "availability": { "$ref": "#/components/schemas/CheckpointFileAvailability" }
                    }
                },
                "CheckpointView": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["checkpoint_id", "checkpoint_digest", "turn_index", "files", "unknown_mutation_count", "fully_restorable"],
                    "properties": {
                        "checkpoint_id": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "checkpoint_digest": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "turn_index": { "type": "integer", "format": "uint64", "minimum": 1 },
                        "prompt": { "type": ["string", "null"], "maxLength": 32768 },
                        "files": { "type": "array", "maxItems": 256, "items": { "$ref": "#/components/schemas/CheckpointFileView" } },
                        "unknown_mutation_count": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "fully_restorable": { "type": "boolean" }
                    }
                },
                "ConversationForkPointView": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["source_turn_index", "source_turn_digest", "source_boundary_stream_sequence", "source_finalized_stream_sequence"],
                    "properties": {
                        "source_turn_index": { "type": "integer", "format": "uint64", "minimum": 1 },
                        "source_turn_digest": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "source_boundary_stream_sequence": { "type": "integer", "format": "uint64", "minimum": 1 },
                        "source_finalized_stream_sequence": { "type": "integer", "format": "uint64", "minimum": 1 }
                    }
                },
                "ConversationRecoveryView": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["checkpoints", "fork_points", "through_stream_sequence"],
                    "properties": {
                        "checkpoints": { "type": "array", "maxItems": 256, "items": { "$ref": "#/components/schemas/CheckpointView" } },
                        "fork_points": { "type": "array", "maxItems": 256, "items": { "$ref": "#/components/schemas/ConversationForkPointView" } },
                        "through_stream_sequence": { "type": "integer", "format": "uint64", "minimum": 0 }
                    }
                },
                "CheckpointRestoreRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["checkpoint_id", "checkpoint_digest"],
                    "properties": {
                        "checkpoint_id": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "checkpoint_digest": { "type": "string", "minLength": 1, "maxLength": 512 }
                    }
                },
                "CompactionEconomics": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["before_input_tokens", "target_input_tokens", "context_window_tokens", "output_tokens", "safety_buffer_tokens", "savings_tokens", "savings_ratio_ppm", "minimum_savings_tokens", "minimum_savings_ratio_ppm", "summary_cache_read_tokens", "summary_uncached_input_tokens", "summary_output_tokens"],
                    "properties": {
                        "before_input_tokens": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "target_input_tokens": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "context_window_tokens": { "type": "integer", "format": "uint64", "minimum": 1 },
                        "output_tokens": { "type": "integer", "format": "uint64", "minimum": 1 },
                        "safety_buffer_tokens": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "savings_tokens": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "savings_ratio_ppm": { "type": "integer", "format": "uint32", "minimum": 0 },
                        "minimum_savings_tokens": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "minimum_savings_ratio_ppm": { "type": "integer", "format": "uint32", "minimum": 0 },
                        "summary_cache_read_tokens": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "summary_uncached_input_tokens": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "summary_output_tokens": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "summary_cost_nano_usd": { "type": "integer", "format": "uint64", "minimum": 0 }
                    }
                },
                "CompactionAdmissionReady": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["kind", "economics"],
                    "properties": {
                        "kind": { "type": "string", "const": "ready" },
                        "economics": { "$ref": "#/components/schemas/CompactionEconomics" }
                    }
                },
                "CompactionAdmissionPrepared": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["kind", "standalone_tool_output_shrink_available"],
                    "properties": {
                        "kind": { "type": "string", "const": "prepared" },
                        "standalone_tool_output_shrink_available": { "type": "boolean" }
                    }
                },
                "CompactionAdmissionNoHistory": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["kind", "durable_message_count", "minimum_tail_turn_count"],
                    "properties": {
                        "kind": { "type": "string", "const": "no_foldable_history" },
                        "durable_message_count": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "minimum_tail_turn_count": { "type": "integer", "format": "uint64", "minimum": 1 }
                    }
                },
                "CompactionAdmissionUnavailable": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["kind", "reason"],
                    "properties": {
                        "kind": { "type": "string", "const": "unavailable" },
                        "reason": { "type": "string", "maxLength": 4096 }
                    }
                },
                "CompactionPolicy": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["strategy", "phase", "native_carrier_available"],
                    "properties": {
                        "strategy": { "type": "string", "const": "cache_aware_v3" },
                        "phase": { "type": "string", "enum": ["below_observe", "observe", "prepare", "admit", "emergency"] },
                        "forecast_confidence": { "oneOf": [
                            { "type": "string", "enum": ["low", "medium", "high"] },
                            { "type": "null" }
                        ] },
                        "admission_reason": { "oneOf": [
                            { "type": "string", "enum": ["emergency_fit", "projected_fit_required", "qualified_cost_savings", "pricing_unavailable", "low_forecast_confidence", "expected_turns_before_break_even", "insufficient_savings"] },
                            { "type": "null" }
                        ] },
                        "native_carrier_available": { "type": "boolean" }
                    }
                },
                "CompactionConstraint": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["text", "source_event_id", "source_field_path"],
                    "properties": {
                        "text": { "type": "string", "minLength": 1, "maxLength": 16384 },
                        "source_event_id": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "source_field_path": { "type": "string", "minLength": 1, "maxLength": 512 }
                    }
                },
                "CompactionToolArtifact": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": [
                        "source_event_id",
                        "content_sha256",
                        "tool_name",
                        "tool_call_id",
                        "status",
                        "original_content_bytes",
                        "original_content_token_upper_bound",
                        "head_excerpt",
                        "tail_excerpt",
                        "reason",
                        "recovery_instruction"
                    ],
                    "properties": {
                        "source_event_id": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "content_sha256": { "type": "string", "pattern": "^sha256:[0-9a-f]{64}$" },
                        "tool_name": { "type": "string", "minLength": 1, "maxLength": 256 },
                        "tool_call_id": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "status": { "type": "string", "minLength": 1, "maxLength": 64 },
                        "original_content_bytes": { "type": "integer", "format": "uint64", "minimum": 1 },
                        "original_content_token_upper_bound": { "type": "integer", "format": "uint64", "minimum": 1 },
                        "head_excerpt": { "type": "string", "maxLength": 513 },
                        "tail_excerpt": { "type": "string", "maxLength": 513 },
                        "reason": { "type": "string", "const": "large_completed_historical_result" },
                        "recovery_instruction": { "type": "string", "minLength": 1, "maxLength": 513 }
                    }
                },
                "CompactionDetails": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": [
                        "active_objective",
                        "objective_source_event_id",
                        "active_constraints",
                        "folded_complete_turn_count",
                        "folded_token_upper_bound",
                        "retained_complete_turn_count",
                        "retained_token_upper_bound",
                        "tool_artifact_count",
                        "tool_artifacts",
                        "pending_work_count",
                        "unresolved_question_count",
                        "recoverable_attachment_count",
                        "protected_control_event_count",
                        "protected_active_tool_or_approval_count"
                    ],
                    "properties": {
                        "active_objective": { "type": "string", "minLength": 1, "maxLength": 16384 },
                        "objective_source_event_id": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "active_constraints": {
                            "type": "array",
                            "maxItems": 128,
                            "items": { "$ref": "#/components/schemas/CompactionConstraint" }
                        },
                        "folded_complete_turn_count": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "folded_token_upper_bound": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "retained_complete_turn_count": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "retained_token_upper_bound": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "tool_artifact_count": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "tool_artifacts": {
                            "type": "array",
                            "maxItems": 16,
                            "items": { "$ref": "#/components/schemas/CompactionToolArtifact" }
                        },
                        "pending_work_count": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "unresolved_question_count": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "recoverable_attachment_count": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "protected_control_event_count": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "protected_active_tool_or_approval_count": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "current_cache_read_tokens": { "type": ["integer", "null"], "format": "uint64", "minimum": 0 },
                        "break_even_turns": { "type": ["integer", "null"], "format": "uint32", "minimum": 1 }
                    }
                },
                "CompactionReview": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["folded_event_count", "retained_event_count", "policy", "admission"],
                    "properties": {
                        "preview_id": { "type": ["string", "null"], "maxLength": 512 },
                        "folded_event_count": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "retained_event_count": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "policy": { "$ref": "#/components/schemas/CompactionPolicy" },
                        "details": { "oneOf": [
                            { "$ref": "#/components/schemas/CompactionDetails" },
                            { "type": "null" }
                        ] },
                        "admission": { "oneOf": [
                            { "$ref": "#/components/schemas/CompactionAdmissionPrepared" },
                            { "$ref": "#/components/schemas/CompactionAdmissionReady" },
                            { "$ref": "#/components/schemas/CompactionAdmissionNoHistory" },
                            { "$ref": "#/components/schemas/CompactionAdmissionUnavailable" }
                        ] }
                    }
                },
                "CheckpointRestoreConflictReason": {
                    "type": "string",
                    "enum": ["workspace_mismatch", "current_hash_mismatch", "intent_state_conflict", "artifact_unavailable", "sensitive_snapshot", "unsupported_snapshot", "invalid_binding"]
                },
                "CheckpointRestorePreviewFile": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["path", "restore_kind"],
                    "properties": {
                        "path": { "type": "string", "maxLength": 4096 },
                        "restore_kind": { "$ref": "#/components/schemas/CheckpointRestoreKind" },
                        "expected_current_hash": { "type": ["string", "null"], "maxLength": 512 },
                        "actual_current_hash": { "type": ["string", "null"], "maxLength": 512 },
                        "conflict_reason": { "oneOf": [{ "$ref": "#/components/schemas/CheckpointRestoreConflictReason" }, { "type": "null" }] }
                    }
                },
                "CheckpointReverseDiff": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["path", "diff", "truncated", "original_line_count"],
                    "properties": {
                        "path": { "type": "string", "maxLength": 4096 },
                        "diff": { "type": "string", "maxLength": 65536 },
                        "truncated": { "type": "boolean" },
                        "original_line_count": { "type": "integer", "format": "uint64", "minimum": 0 }
                    }
                },
                "CheckpointRestoreReview": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["checkpoint_id", "checkpoint_digest", "files", "reverse_diffs", "unknown_mutation_count", "ready"],
                    "properties": {
                        "checkpoint_id": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "checkpoint_digest": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "files": { "type": "array", "maxItems": 256, "items": { "$ref": "#/components/schemas/CheckpointRestorePreviewFile" } },
                        "reverse_diffs": { "type": "array", "maxItems": 256, "items": { "$ref": "#/components/schemas/CheckpointReverseDiff" } },
                        "unknown_mutation_count": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "ready": { "type": "boolean" }
                    }
                },
                "ConversationRecoveryRestoreAction": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["kind", "checkpoint_id", "checkpoint_digest"],
                    "properties": {
                        "kind": { "type": "string", "const": "restore_checkpoint" },
                        "checkpoint_id": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "checkpoint_digest": { "type": "string", "minLength": 1, "maxLength": 512 }
                    }
                },
                "ConversationRecoveryCompactionAction": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["kind", "preview_id"],
                    "properties": {
                        "kind": { "type": "string", "const": "apply_compaction" },
                        "preview_id": { "type": "string", "minLength": 1, "maxLength": 512 }
                    }
                },
                "ConversationRecoveryPrepareCompactionAction": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["kind", "preview_id"],
                    "properties": {
                        "kind": { "type": "string", "const": "prepare_compaction" },
                        "preview_id": { "type": "string", "minLength": 1, "maxLength": 512 }
                    }
                },
                "ConversationRecoveryToolOutputShrinkAction": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["kind", "preview_id"],
                    "properties": {
                        "kind": { "type": "string", "const": "apply_standalone_tool_output_shrink" },
                        "preview_id": { "type": "string", "minLength": 1, "maxLength": 512 }
                    }
                },
                "ConversationRecoveryForkAction": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["kind", "source_turn_digest", "model_ref"],
                    "properties": {
                        "kind": { "type": "string", "const": "fork_conversation" },
                        "source_turn_digest": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "model_ref": { "$ref": "#/components/schemas/ProviderModelRef" }
                    }
                },
                "ConversationRecoveryCommandAction": {
                    "oneOf": [
                        { "$ref": "#/components/schemas/ConversationRecoveryPrepareCompactionAction" },
                        { "$ref": "#/components/schemas/ConversationRecoveryCompactionAction" },
                        { "$ref": "#/components/schemas/ConversationRecoveryToolOutputShrinkAction" },
                        { "$ref": "#/components/schemas/ConversationRecoveryRestoreAction" },
                        { "$ref": "#/components/schemas/ConversationRecoveryForkAction" }
                    ]
                },
                "ConversationRecoveryCommand": {
                    "allOf": [
                        { "$ref": "#/components/schemas/CommandEnvelopeBase" },
                        {
                            "type": "object",
                            "required": ["payload"],
                            "properties": { "payload": { "$ref": "#/components/schemas/ConversationRecoveryCommandAction" } }
                        }
                    ]
                },
                "CheckpointRestoreReceipt": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["checkpoint_id", "batch_id", "restored_file_count", "verification_stale"],
                    "properties": {
                        "checkpoint_id": { "type": "string" },
                        "batch_id": { "type": "string" },
                        "restored_file_count": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "verification_stale": { "type": "boolean" }
                    }
                },
                "CompactionReceipt": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["compaction_id", "attempt_id", "task_memory_id", "folded_event_count", "tool_output_projection_recorded", "native_carrier_materialized"],
                    "properties": {
                        "compaction_id": { "type": "string", "maxLength": 512 },
                        "attempt_id": { "type": "string", "maxLength": 512 },
                        "task_memory_id": { "type": "string", "maxLength": 512 },
                        "folded_event_count": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "tool_output_projection_recorded": { "type": "boolean" },
                        "native_carrier_materialized": { "type": "boolean" },
                        "native_carrier_status": { "type": "string" }
                    }
                },
                "ToolOutputShrinkReceipt": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["context_epoch_id", "projected_output_count"],
                    "properties": {
                        "context_epoch_id": { "type": "string", "maxLength": 512 },
                        "projected_output_count": { "type": "integer", "format": "uint64", "minimum": 1 }
                    }
                },
                "ConversationForkReceipt": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["session_ref", "session_id", "copied_message_count", "copied_external_provenance_count"],
                    "properties": {
                        "session_ref": { "type": "string", "maxLength": 512 },
                        "session_id": { "type": "string", "maxLength": 512 },
                        "copied_message_count": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "copied_external_provenance_count": { "type": "integer", "format": "uint64", "minimum": 0 }
                    }
                },
                "ConversationRecoveryCommandReceipt": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["command_id", "client_id", "session_id", "action", "recovery", "replayed"],
                    "properties": {
                        "command_id": { "type": "string" },
                        "client_id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "action": { "type": "string", "enum": ["prepare_compaction", "apply_compaction", "apply_standalone_tool_output_shrink", "restore_checkpoint", "fork_conversation"] },
                        "compaction": { "oneOf": [{ "$ref": "#/components/schemas/CompactionReceipt" }, { "type": "null" }] },
                        "compaction_review": { "oneOf": [{ "$ref": "#/components/schemas/CompactionReview" }, { "type": "null" }] },
                        "tool_output_shrink": { "oneOf": [{ "$ref": "#/components/schemas/ToolOutputShrinkReceipt" }, { "type": "null" }] },
                        "restore": { "oneOf": [{ "$ref": "#/components/schemas/CheckpointRestoreReceipt" }, { "type": "null" }] },
                        "fork": { "oneOf": [{ "$ref": "#/components/schemas/ConversationForkReceipt" }, { "type": "null" }] },
                        "recovery": { "$ref": "#/components/schemas/ConversationRecoveryView" },
                        "correlation_id": { "type": ["string", "null"] },
                        "replayed": { "type": "boolean" }
                    }
                },
                "DecimalSequence": {
                    "type": "string",
                    "maxLength": 20,
                    "pattern": "^(?:0|[1-9][0-9]{0,18}|1[0-7][0-9]{18}|18[0-3][0-9]{17}|184[0-3][0-9]{16}|1844[0-5][0-9]{15}|18446[0-6][0-9]{14}|184467[0-3][0-9]{13}|1844674[0-3][0-9]{12}|184467440[0-6][0-9]{10}|1844674407[0-2][0-9]{9}|18446744073[0-6][0-9]{8}|1844674407370[0-8][0-9]{6}|18446744073709[0-4][0-9]{5}|184467440737095[0-4][0-9]{4}|18446744073709550[0-9]{3}|18446744073709551[0-5][0-9]{2}|1844674407370955160[0-9]|1844674407370955161[0-4]|18446744073709551615)$"
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
                            "enum": ["ready", "oversized", "scan_budget_exceeded", "invalid"]
                        },
                        "source_diagnostic": {
                            "type": ["string", "null"],
                            "enum": ["unsafe_source", "invalid_event_stream", "invalid_projection", "missing_session_identity", null]
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
                        "model_ref": { "oneOf": [{ "$ref": "#/components/schemas/ProviderModelRef" }, { "type": "null" }] },
                        "model_selection_binding": { "type": ["string", "null"] },
                        "route_recovery_binding": { "type": ["string", "null"] },
                        "reasoning_effort": { "oneOf": [{ "$ref": "#/components/schemas/ReasoningEffort" }, { "type": "null" }] },
                        "reasoning_effort_binding": { "type": ["string", "null"] },
                        "skill_binding": { "oneOf": [{ "$ref": "#/components/schemas/ApplicationSkillBinding" }, { "type": "null" }] },
                        "agent_binding": { "oneOf": [{ "$ref": "#/components/schemas/ApplicationAgentBinding" }, { "type": "null" }] },
                        "task_continuation": { "oneOf": [{ "$ref": "#/components/schemas/TaskContinuationRequest" }, { "type": "null" }] }
                    }
                },
                "TaskContinuationRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["task_id"],
                    "properties": {
                        "task_id": { "type": "string", "minLength": 1 },
                        "guidance": { "type": ["string", "null"], "minLength": 1 }
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
                    "required": ["model_ref", "display_name", "availability", "recommendation", "provenance", "model_name", "available_reasoning_efforts"],
                    "properties": {
                        "model_ref": { "$ref": "#/components/schemas/ProviderModelRef" },
                        "display_name": { "type": "string" },
                        "availability": { "type": "string", "enum": ["available", "unverified", "configured_unavailable"] },
                        "recommendation": { "type": "string", "enum": ["recommended", "standard"] },
                        "provenance": { "type": "string", "enum": ["remote", "cache", "bundled", "configured", "manual"] },
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
                    "required": ["model_ref", "provider_name", "model_name", "model_options", "model_selection", "model_selection_binding", "default_permission_mode", "available_permission_modes", "available_reasoning_efforts", "context_window_source", "extension_catalog"],
                    "properties": {
                        "model_ref": { "$ref": "#/components/schemas/ProviderModelRef" },
                        "provider_name": { "type": "string" },
                        "model_name": { "type": "string" },
                        "model_options": {
                            "type": "array",
                            "minItems": 1,
                            "items": { "$ref": "#/components/schemas/ApplicationModelOption" }
                        },
                        "model_selection": { "type": "string", "enum": ["same_session"] },
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
                        "context_window_source": { "type": "string", "enum": ["connection", "provider", "config", "unavailable"] },
                        "extension_catalog": { "$ref": "#/components/schemas/ApplicationExtensionCatalog" },
                        "route_recovery": { "oneOf": [{ "$ref": "#/components/schemas/SessionRouteRecoveryView" }, { "type": "null" }] }
                    }
                },
                "SessionRouteRecoveryView": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["code", "allowed_actions", "recovery_binding", "retryable"],
                    "properties": {
                        "code": {
                            "type": "string",
                            "enum": ["session_route_confirmation_required", "session_route_selection_required", "model_route_not_configured", "connection_config_invalid", "provider_unavailable", "session_already_active", "session_writer_busy", "session_stream_invalid"]
                        },
                        "allowed_actions": {
                            "type": "array",
                            "uniqueItems": true,
                            "items": {
                                "type": "string",
                                "enum": ["confirm_current_route", "repair_connection", "select_replacement", "start_new_session", "retry_provider", "retry_session_attach", "back_to_session_library"]
                            }
                        },
                        "recovery_binding": { "type": "string", "minLength": 1, "maxLength": 128 },
                        "retryable": { "type": "boolean" }
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
                    "enum": ["preview_compaction", "open_intent_stack", "new_session", "focus_effort", "focus_model", "open_session_picker", "open_agent_workbench", "open_settings", "open_support"]
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
                "TerminalTaskCancelCommand": {
                    "allOf": [
                        { "$ref": "#/components/schemas/CommandEnvelopeBase" },
                        {
                            "type": "object",
                            "required": ["payload"],
                            "properties": {
                                "payload": { "$ref": "#/components/schemas/TerminalTaskCancelRequest" }
                            }
                        }
                    ]
                },
                "TerminalTaskCancelRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["task_id", "expected_generation"],
                    "properties": {
                        "task_id": { "type": "string" },
                        "expected_generation": { "type": "integer", "format": "uint64" }
                    }
                },
                "TerminalTaskCancelCommandReceipt": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["command_id", "client_id", "session_id", "run_id", "terminal_task", "replayed"],
                    "properties": {
                        "command_id": { "type": "string" },
                        "client_id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "expected_stream_sequence": { "type": ["integer", "null"], "format": "uint64" },
                        "correlation_id": { "type": ["string", "null"] },
                        "run_id": { "type": "string" },
                        "terminal_task": { "$ref": "#/components/schemas/TerminalLifecycle" },
                        "replayed": { "type": "boolean" }
                    }
                },
                "TaskPauseCommand": {
                    "allOf": [
                        { "$ref": "#/components/schemas/CommandEnvelopeBase" },
                        {
                            "type": "object",
                            "required": ["payload"],
                            "properties": {
                                "payload": { "$ref": "#/components/schemas/TaskPauseRequest" }
                            }
                        }
                    ]
                },
                "TaskPauseRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["request_id", "task_id", "plan_version"],
                    "properties": {
                        "request_id": { "type": "string" },
                        "task_id": { "type": "string" },
                        "plan_version": { "type": "integer", "format": "uint32", "minimum": 1 }
                    }
                },
                "TaskPauseCommandReceipt": {
                    "type": "object",
                    "required": ["command_id", "client_id", "session_id", "task_id", "plan_version", "run", "replayed"],
                    "properties": {
                        "command_id": { "type": "string" },
                        "client_id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "expected_stream_sequence": { "type": ["integer", "null"], "format": "uint64" },
                        "correlation_id": { "type": ["string", "null"] },
                        "task_id": { "type": "string" },
                        "plan_version": { "type": "integer", "format": "uint32" },
                        "run": { "$ref": "#/components/schemas/RunSnapshot" },
                        "replayed": { "type": "boolean" }
                    }
                },
                "RunSnapshot": {
                    "type": "object",
                    "required": ["id", "session_id", "status", "permission_mode", "prompt_preview", "pending_approvals", "terminal_tasks", "stream_sequence"],
                    "properties": {
                        "id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "status": { "$ref": "#/components/schemas/RunStatus" },
                        "permission_mode": { "$ref": "#/components/schemas/PermissionMode" },
                        "reasoning_effort": { "oneOf": [{ "$ref": "#/components/schemas/ReasoningEffort" }, { "type": "null" }] },
                        "prompt_preview": { "type": "string" },
                        "stream_sequence": { "type": "integer", "format": "uint64" },
                        "pending_approvals": { "type": "array", "maxItems": 8, "items": { "$ref": "#/components/schemas/PendingApproval" } },
                        "terminal_tasks": { "type": "array", "items": { "$ref": "#/components/schemas/TerminalLifecycle" } }
                    }
                },
                "TerminalReadinessKind": {
                    "type": "string",
                    "enum": ["none", "output_contains", "output_regex"]
                },
                "TerminalTaskStatus": {
                    "oneOf": [
                        { "type": "object", "additionalProperties": false, "required": ["state"], "properties": { "state": { "const": "starting" } } },
                        { "type": "object", "additionalProperties": false, "required": ["state"], "properties": { "state": { "const": "running" } } },
                        { "type": "object", "additionalProperties": false, "required": ["state"], "properties": { "state": { "const": "exited" }, "exit_code": { "type": ["integer", "null"], "format": "int32" } } },
                        { "type": "object", "additionalProperties": false, "required": ["state", "reason"], "properties": { "state": { "const": "failed" }, "reason": { "type": "string" } } },
                        { "type": "object", "additionalProperties": false, "required": ["state"], "properties": { "state": { "const": "cancelled" } } },
                        { "type": "object", "additionalProperties": false, "required": ["state"], "properties": { "state": { "const": "interrupted" } } }
                    ]
                },
                "TerminalReadinessStatus": {
                    "oneOf": [
                        { "type": "object", "additionalProperties": false, "required": ["state"], "properties": { "state": { "const": "none" } } },
                        { "type": "object", "additionalProperties": false, "required": ["state", "kind"], "properties": { "state": { "const": "waiting" }, "kind": { "$ref": "#/components/schemas/TerminalReadinessKind" } } },
                        { "type": "object", "additionalProperties": false, "required": ["state", "kind", "ready_at_ms"], "properties": { "state": { "const": "ready" }, "kind": { "$ref": "#/components/schemas/TerminalReadinessKind" }, "ready_at_ms": { "type": "integer", "format": "uint64" } } },
                        { "type": "object", "additionalProperties": false, "required": ["state", "kind", "reason"], "properties": { "state": { "const": "failed" }, "kind": { "$ref": "#/components/schemas/TerminalReadinessKind" }, "reason": { "type": "string" } } },
                        { "type": "object", "additionalProperties": false, "required": ["state", "kind"], "properties": { "state": { "const": "timed_out" }, "kind": { "$ref": "#/components/schemas/TerminalReadinessKind" } } }
                    ]
                },
                "TerminalLifecycle": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["task_id", "generation", "status", "readiness", "total_output_bytes", "emitted_at_ms"],
                    "properties": {
                        "task_id": { "type": "string" },
                        "generation": { "type": "integer", "format": "uint64" },
                        "status": { "$ref": "#/components/schemas/TerminalTaskStatus" },
                        "readiness": { "$ref": "#/components/schemas/TerminalReadinessStatus" },
                        "total_output_bytes": { "type": "integer", "format": "uint64" },
                        "emitted_at_ms": { "type": "integer", "format": "uint64" }
                    }
                },
                "RunStatus": {
                    "type": "string",
                    "enum": ["starting", "running", "waiting_for_approval", "cancel_requested", "pause_requested", "execution_uncertain", "finished", "failed", "cancelled", "paused", "interrupted"]
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
                        "family_pattern": { "type": ["string", "null"], "maxLength": 2048 },
                        "reason": { "type": ["string", "null"] }
                    }
                },
                "ApprovalDecision": {
                    "type": "string",
                    "enum": ["approve", "approve_for_session", "approve_for_family", "deny"]
                },
                "ApprovalCommandReceipt": {
                    "type": "object",
                    "required": ["command_id", "client_id", "session_id", "run_id", "call_id", "approval_request_id", "decision", "route_state", "registry_revision", "replayed"],
                    "properties": {
                        "command_id": { "type": "string" },
                        "client_id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "run_id": { "type": "string" },
                        "call_id": { "type": "string" },
                        "approval_request_id": { "type": "string" },
                        "expected_stream_sequence": { "type": ["integer", "null"], "format": "uint64" },
                        "correlation_id": { "type": ["string", "null"] },
                        "decision": { "$ref": "#/components/schemas/ApprovalDecisionRecord" },
                        "route_state": { "$ref": "#/components/schemas/ApprovalRouteState" },
                        "registry_revision": { "type": "integer", "format": "uint64" },
                        "replayed": { "type": "boolean" }
                    }
                },
                "ApprovalRouteState": {
                    "type": "string",
                    "enum": ["decision_accepted", "delivery_uncertain", "terminal"]
                },
                "ApprovalDecisionRecord": {
                    "type": "object",
                    "required": ["run_id", "call_id", "decision"],
                    "properties": {
                        "run_id": { "type": "string" },
                        "call_id": { "type": "string" },
                        "decision": { "type": "string", "enum": ["approved", "approved_for_session", "denied"] },
                        "reason": { "type": ["string", "null"] }
                    }
                },
                "IntentVersionRef": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["intent_id", "version"],
                    "properties": {
                        "intent_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                        "version": { "type": "integer", "format": "uint64", "minimum": 1 }
                    }
                },
                "IntentAcceptanceCriterion": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["criterion_id", "statement", "required"],
                    "properties": {
                        "criterion_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                        "statement": { "type": "string", "maxLength": 4096 },
                        "required": { "type": "boolean" }
                    }
                },
                "IntentDefinitionState": {
                    "type": "string",
                    "enum": ["proposed", "accepted", "superseded", "invalid"]
                },
                "IntentApplicationState": {
                    "type": "string",
                    "enum": ["unapplied", "applied", "dropped", "needs_review", "needs_rebuild", "read_only", "out_of_scope"]
                },
                "IntentAuthorityState": {
                    "type": "string",
                    "enum": ["active", "read_only_provenance", "out_of_scope"]
                },
                "IntentArtifactKind": {
                    "type": "string",
                    "enum": ["file_hunk", "test_evidence", "documentation", "change_set", "verification_receipt", "unsupported_side_effect"]
                },
                "IntentArtifactOwnership": {
                    "type": "string",
                    "enum": ["exclusive", "shared", "unowned", "drifted"]
                },
                "IntentArtifactAvailability": {
                    "type": "string",
                    "enum": ["available", "deleted", "expired", "corrupted"]
                },
                "IntentOperationKind": {
                    "type": "string",
                    "enum": ["drop", "revise_impact_preview", "replace_impact_preview", "adopt"]
                },
                "IntentOperationResolution": {
                    "type": "string",
                    "enum": ["committed", "rejected", "cancelled", "conflicted", "partially_applied", "interrupted"]
                },
                "IntentOperationErrorCode": {
                    "type": "string",
                    "enum": [
                        "unsupported_schema",
                        "unknown_intent",
                        "unknown_operation",
                        "stale_intent_version",
                        "stale_stack_version",
                        "invalid_dependency_graph",
                        "target_not_leaf",
                        "shared_artifact",
                        "unowned_artifact",
                        "drifted_artifact",
                        "artifact_unavailable",
                        "artifact_digest_mismatch",
                        "unsupported_artifact",
                        "unsupported_side_effect",
                        "missing_execution_lineage",
                        "missing_parent_mutation_evidence",
                        "missing_current_verification_evidence",
                        "preview_digest_mismatch",
                        "workspace_revision_mismatch",
                        "permission_denied",
                        "approval_authority_unavailable",
                        "workspace_lease_unavailable",
                        "workspace_out_of_scope",
                        "operation_state_conflict",
                        "intent_state_conflict",
                        "partial_application",
                        "reconciliation_required"
                    ]
                },
                "IntentOperationFileAction": {
                    "type": "string",
                    "enum": ["create", "update", "delete"]
                },
                "IntentVerificationImpact": {
                    "type": "string",
                    "enum": ["becomes_stale", "rerun_required"]
                },
                "IntentSource": {
                    "oneOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["kind", "source_turn_id"],
                            "properties": {
                                "kind": { "type": "string", "const": "user_turn" },
                                "source_turn_id": { "type": "string", "maxLength": 512 }
                            }
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["kind", "source_turn_id"],
                            "properties": {
                                "kind": { "type": "string", "const": "accepted_suggestion" },
                                "source_turn_id": { "type": "string", "maxLength": 512 }
                            }
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["kind", "safe_source_label"],
                            "properties": {
                                "kind": { "type": "string", "const": "trusted_spec" },
                                "safe_source_label": { "type": "string", "maxLength": 4096 }
                            }
                        }
                    ]
                },
                "IntentArtifactSummary": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["artifact_id", "artifact_kind", "ownership", "availability"],
                    "properties": {
                        "artifact_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                        "artifact_kind": { "$ref": "#/components/schemas/IntentArtifactKind" },
                        "ownership": { "$ref": "#/components/schemas/IntentArtifactOwnership" },
                        "availability": { "$ref": "#/components/schemas/IntentArtifactAvailability" },
                        "normalized_relative_path": { "type": ["string", "null"], "maxLength": 4096 }
                    }
                },
                "IntentConflict": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["code", "safe_reason"],
                    "properties": {
                        "code": { "$ref": "#/components/schemas/IntentOperationErrorCode" },
                        "intent_ref": { "oneOf": [{ "$ref": "#/components/schemas/IntentVersionRef" }, { "type": "null" }] },
                        "artifact_id": { "type": ["string", "null"], "maxLength": 128 },
                        "safe_reason": { "type": "string", "maxLength": 2048 }
                    }
                },
                "Intent": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": [
                        "intent_ref",
                        "title",
                        "statement",
                        "acceptance_criteria",
                        "depends_on",
                        "source",
                        "definition_state",
                        "application_state",
                        "exclusive_artifact_count",
                        "shared_artifact_count",
                        "unowned_artifact_count",
                        "drifted_artifact_count",
                        "unavailable_artifact_count",
                        "advisory_criterion_count",
                        "system_verified_criterion_count",
                        "artifacts",
                        "available_actions"
                    ],
                    "properties": {
                        "intent_ref": { "$ref": "#/components/schemas/IntentVersionRef" },
                        "title": { "type": "string", "maxLength": 256 },
                        "statement": { "type": "string", "maxLength": 4096 },
                        "acceptance_criteria": { "type": "array", "maxItems": 64, "items": { "$ref": "#/components/schemas/IntentAcceptanceCriterion" } },
                        "depends_on": { "type": "array", "maxItems": 64, "items": { "type": "string", "maxLength": 128 } },
                        "source": { "$ref": "#/components/schemas/IntentSource" },
                        "definition_state": { "$ref": "#/components/schemas/IntentDefinitionState" },
                        "application_state": { "$ref": "#/components/schemas/IntentApplicationState" },
                        "exclusive_artifact_count": { "type": "integer", "format": "uint32", "minimum": 0 },
                        "shared_artifact_count": { "type": "integer", "format": "uint32", "minimum": 0 },
                        "unowned_artifact_count": { "type": "integer", "format": "uint32", "minimum": 0 },
                        "drifted_artifact_count": { "type": "integer", "format": "uint32", "minimum": 0 },
                        "unavailable_artifact_count": { "type": "integer", "format": "uint32", "minimum": 0 },
                        "advisory_criterion_count": { "type": "integer", "format": "uint32", "minimum": 0 },
                        "system_verified_criterion_count": { "type": "integer", "format": "uint32", "minimum": 0 },
                        "artifacts": { "type": "array", "items": { "$ref": "#/components/schemas/IntentArtifactSummary" } },
                        "available_actions": { "type": "array", "uniqueItems": true, "items": { "$ref": "#/components/schemas/IntentOperationKind" } }
                    }
                },
                "IntentStack": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["schema_version", "stack_id", "stack_version", "authority_state", "plan_digest", "intents", "conflicts"],
                    "properties": {
                        "schema_version": { "type": "integer", "const": 1 },
                        "stack_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                        "stack_version": { "type": "integer", "format": "uint64", "minimum": 1 },
                        "authority_state": { "$ref": "#/components/schemas/IntentAuthorityState" },
                        "plan_digest": { "type": "string", "pattern": "^sha256:jcs-v1:[0-9a-f]{64}$" },
                        "intents": { "type": "array", "maxItems": 64, "items": { "$ref": "#/components/schemas/Intent" } },
                        "conflicts": { "type": "array", "items": { "$ref": "#/components/schemas/IntentConflict" } }
                    }
                },
                "IntentStackState": {
                    "oneOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["status", "schema_version", "stack"],
                            "properties": {
                                "status": { "type": "string", "const": "available" },
                                "schema_version": { "type": "integer", "const": 1 },
                                "stack": { "$ref": "#/components/schemas/IntentStack" }
                            }
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["status", "schema_version", "safe_message"],
                            "properties": {
                                "status": { "type": "string", "const": "not_created" },
                                "schema_version": { "type": "integer", "const": 1 },
                                "safe_message": { "type": "string", "maxLength": 2048 }
                            }
                        }
                    ]
                },
                "IntentDropPreviewRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["intent_ref"],
                    "properties": {
                        "intent_ref": { "$ref": "#/components/schemas/IntentVersionRef" }
                    }
                },
                "IntentOperationFileSummary": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["normalized_relative_path", "action", "artifact_ids"],
                    "properties": {
                        "normalized_relative_path": { "type": "string", "maxLength": 4096 },
                        "action": { "$ref": "#/components/schemas/IntentOperationFileAction" },
                        "artifact_ids": { "type": "array", "items": { "type": "string", "minLength": 1, "maxLength": 128 } }
                    }
                },
                "IntentVerificationImpactSummary": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["receipt_id", "impact"],
                    "properties": {
                        "receipt_id": { "type": "string", "maxLength": 512 },
                        "impact": { "$ref": "#/components/schemas/IntentVerificationImpact" }
                    }
                },
                "IntentOperationPreview": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["schema_version", "operation_id", "operation_kind", "stack_id", "stack_version", "target_intents", "target_is_leaf", "workspace_revision", "file_effects", "retained_intents", "verification_impacts", "conflicts", "preview_digest"],
                    "properties": {
                        "schema_version": { "type": "integer", "const": 1 },
                        "operation_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                        "operation_kind": { "$ref": "#/components/schemas/IntentOperationKind" },
                        "stack_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                        "stack_version": { "type": "integer", "format": "uint64", "minimum": 1 },
                        "target_intents": { "type": "array", "minItems": 1, "items": { "$ref": "#/components/schemas/IntentVersionRef" } },
                        "target_is_leaf": { "type": "boolean" },
                        "workspace_revision": { "type": "integer", "format": "uint64", "minimum": 0 },
                        "expires_at_ms": { "type": ["integer", "null"], "format": "uint64", "minimum": 1 },
                        "file_effects": { "type": "array", "items": { "$ref": "#/components/schemas/IntentOperationFileSummary" } },
                        "retained_intents": { "type": "array", "items": { "$ref": "#/components/schemas/IntentVersionRef" } },
                        "verification_impacts": { "type": "array", "items": { "$ref": "#/components/schemas/IntentVerificationImpactSummary" } },
                        "conflicts": { "type": "array", "items": { "$ref": "#/components/schemas/IntentConflict" } },
                        "preview_digest": { "type": "string", "pattern": "^sha256:jcs-v1:[0-9a-f]{64}$" }
                    }
                },
                "IntentDropRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["operation_id", "stack_version", "preview_digest"],
                    "properties": {
                        "operation_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                        "stack_version": { "type": "integer", "format": "uint64", "minimum": 1 },
                        "preview_digest": { "type": "string", "pattern": "^sha256:jcs-v1:[0-9a-f]{64}$" }
                    }
                },
                "IntentDropCommand": {
                    "allOf": [
                        { "$ref": "#/components/schemas/CommandEnvelopeBase" },
                        {
                            "type": "object",
                            "required": ["payload"],
                            "properties": {
                                "payload": { "$ref": "#/components/schemas/IntentDropRequest" }
                            }
                        }
                    ]
                },
                "IntentOperationExecution": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["preview", "resolution", "mutation_batch_id", "committed_operation_ids", "result_snapshot_id", "error_code"],
                    "properties": {
                        "preview": { "$ref": "#/components/schemas/IntentOperationPreview" },
                        "resolution": { "$ref": "#/components/schemas/IntentOperationResolution" },
                        "mutation_batch_id": { "type": ["string", "null"], "maxLength": 512 },
                        "committed_operation_ids": { "type": "array", "items": { "type": "string", "maxLength": 512 } },
                        "result_snapshot_id": { "type": ["string", "null"], "maxLength": 512 },
                        "error_code": { "oneOf": [{ "$ref": "#/components/schemas/IntentOperationErrorCode" }, { "type": "null" }] }
                    }
                },
                "IntentDropCommandReceipt": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["command_id", "client_id", "session_id", "execution", "replayed"],
                    "properties": {
                        "command_id": { "type": "string" },
                        "client_id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "correlation_id": { "type": ["string", "null"] },
                        "execution": { "$ref": "#/components/schemas/IntentOperationExecution" },
                        "replayed": { "type": "boolean" }
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
                    "required": ["request_id", "task_id", "plan_version", "step_id", "check_spec_id", "check_spec_hash", "policy_hash", "workspace_snapshot_id"],
                    "properties": {
                        "request_id": { "type": "string", "pattern": "^verification-rerun-[0-9a-f]{64}$" },
                        "task_id": { "type": "string" },
                        "plan_version": { "type": "integer", "format": "uint32", "minimum": 1 },
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
                "TaskIntegrationReviewRequest": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["request_id", "task_id", "plan_id", "plan_version", "preview_digest"],
                    "properties": {
                        "request_id": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "task_id": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "plan_id": { "type": "string", "minLength": 1, "maxLength": 512 },
                        "plan_version": { "type": "integer", "format": "uint32", "minimum": 1 },
                        "preview_digest": { "type": "string", "minLength": 1, "maxLength": 512 }
                    }
                },
                "IntegrationPromotionTargetKind": {
                    "type": "string",
                    "enum": ["workspace_apply", "git_ref_advance"]
                },
                "IntegrationLaneCandidateKind": {
                    "type": "string",
                    "enum": ["managed_ref", "snapshot_workspace"]
                },
                "TaskIntegrationLaneView": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["lane_id", "candidate_kind", "proposal_count", "verification_receipt_count"],
                    "properties": {
                        "lane_id": { "type": "string" },
                        "candidate_kind": { "$ref": "#/components/schemas/IntegrationLaneCandidateKind" },
                        "proposal_count": { "type": "integer", "minimum": 0 },
                        "verification_receipt_count": { "type": "integer", "minimum": 0 }
                    }
                },
                "TaskIntegrationReviewView": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["schema_version", "request", "aggregate_diff", "aggregate_diff_digest", "preview_digest", "policy_digest", "target_kind", "lanes", "child_verification_receipt_count", "lane_verification_receipt_count", "conflict_reasons", "verification_invalidation_count", "parent_verification_pending"],
                    "properties": {
                        "schema_version": { "type": "integer", "const": 1 },
                        "request": { "$ref": "#/components/schemas/TaskIntegrationReviewRequest" },
                        "aggregate_diff": { "type": "string", "maxLength": 4194304 },
                        "aggregate_diff_digest": { "type": "string" },
                        "preview_digest": { "type": "string" },
                        "policy_digest": { "type": "string" },
                        "target_kind": { "$ref": "#/components/schemas/IntegrationPromotionTargetKind" },
                        "lanes": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/TaskIntegrationLaneView" }
                        },
                        "child_verification_receipt_count": { "type": "integer", "minimum": 0 },
                        "lane_verification_receipt_count": { "type": "integer", "minimum": 0 },
                        "conflict_reasons": { "type": "array", "items": { "type": "string" } },
                        "verification_invalidation_count": { "type": "integer", "minimum": 0 },
                        "parent_verification_pending": { "type": "boolean", "const": true }
                    }
                },
                "TaskIntegrationAcceptanceCommand": {
                    "allOf": [
                        { "$ref": "#/components/schemas/CommandEnvelopeBase" },
                        {
                            "type": "object",
                            "required": ["payload"],
                            "properties": {
                                "payload": { "$ref": "#/components/schemas/TaskIntegrationReviewRequest" }
                            }
                        }
                    ]
                },
                "IntegrationPromotionStatus": {
                    "type": "string",
                    "enum": ["prepared", "promoted", "conflict", "stale", "failed", "cancelled"]
                },
                "TaskIntegrationAcceptanceView": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["request", "promotion_status", "can_continue"],
                    "properties": {
                        "request": { "$ref": "#/components/schemas/TaskIntegrationReviewRequest" },
                        "promotion_status": { "$ref": "#/components/schemas/IntegrationPromotionStatus" },
                        "parent_verdict": { "oneOf": [{ "$ref": "#/components/schemas/VerificationVerdict" }, { "type": "null" }] },
                        "can_continue": { "type": "boolean" },
                        "promotion_cleanup_error": { "type": ["string", "null"] },
                        "parent_cleanup_error": { "type": ["string", "null"] }
                    }
                },
                "TaskIntegrationAcceptanceCommandReceipt": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["command_id", "client_id", "session_id", "acceptance", "replayed"],
                    "properties": {
                        "command_id": { "type": "string" },
                        "client_id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "correlation_id": { "type": ["string", "null"] },
                        "acceptance": { "$ref": "#/components/schemas/TaskIntegrationAcceptanceView" },
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
                },
                "RunAdmissionErrorResponse": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["error"],
                    "properties": {
                        "error": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["code", "message", "route_recovery"],
                            "properties": {
                                "code": {
                                    "type": "string",
                                    "enum": ["session_route_confirmation_required", "session_route_selection_required", "model_route_not_configured", "connection_config_invalid", "provider_unavailable", "session_already_active", "session_writer_busy", "session_stream_invalid"]
                                },
                                "message": { "type": "string" },
                                "route_recovery": { "$ref": "#/components/schemas/SessionRouteRecoveryView" }
                            }
                        }
                    }
                }
            }
        }
    });
    document["components"]["schemas"]
        .as_object_mut()
        .expect("OpenAPI schemas must be an object")
        .extend(public_event_schemas());
    document
}

fn public_event_schemas() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "ProtocolEvent".to_owned(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["schema_version", "event_class", "run_event"],
            "properties": {
                "schema_version": { "type": "integer", "const": crate::HTTP_PROTOCOL_EVENT_SCHEMA_VERSION },
                "event_class": { "type": "string", "enum": ["durable", "transient"] },
                "replay_id": { "type": "string" },
                "approval_request": { "$ref": "#/components/schemas/PendingApproval" },
                "provisional_id": { "type": "string", "pattern": "^live-v1:[0-9a-f]{64}$" },
                "run_event": { "$ref": "#/components/schemas/PublicRunEvent" }
            }
        }),
    );
    schemas.insert(
        "SessionGrantUnavailableReasonCode".to_owned(),
        json!({
            "type": "string",
            "enum": [
                "analysis_incomplete",
                "semantic_scope_unavailable",
                "non_grantable_effect",
                "containment_binding_unavailable",
                "policy_decision_not_grantable",
                "no_reusable_approval_facet",
                "network_scope_not_grantable",
                "confirmation_required",
                "snapshot_required",
                "subject_scope_unavailable",
                "risk_not_grantable",
                "external_mutation",
                "operation_not_grantable"
            ]
        }),
    );
    schemas.insert(
        "SessionGrantUnavailableReason".to_owned(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["code"],
            "properties": {
                "code": { "$ref": "#/components/schemas/SessionGrantUnavailableReasonCode" }
            }
        }),
    );
    schemas.insert(
        "PendingApproval".to_owned(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["call_id", "tool_name", "approval_request_id", "tool_call_hash", "policy_version", "expires_at_ms", "session_grant_available", "session_grant_unavailable_reason", "display"],
            "properties": {
                "call_id": { "type": "string" },
                "tool_name": { "type": "string" },
                "approval_request_id": { "type": "string" },
                "tool_call_hash": { "type": "string" },
                "policy_version": { "type": "string" },
                "expires_at_ms": { "type": "integer", "format": "uint64" },
                "session_grant_available": { "type": "boolean" },
                "session_grant_unavailable_reason": {
                    "oneOf": [
                        { "$ref": "#/components/schemas/SessionGrantUnavailableReason" },
                        { "type": "null" }
                    ]
                },
                "display": { "$ref": "#/components/schemas/PendingApprovalDisplay" }
            }
        }),
    );
    schemas.insert(
        "PendingApprovalDisplay".to_owned(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["event_sequence", "effects", "subjects", "analysis_status", "analysis_reason_codes", "analysis_reasons", "containment", "decision_reasons", "safe_summary_title", "safe_summary_detail", "snapshot_required"],
            "properties": {
                "event_sequence": { "type": "integer", "format": "uint64", "minimum": 1 },
                "effects": { "type": "array", "maxItems": 16, "items": { "type": "string", "maxLength": 2048 } },
                "subjects": { "type": "array", "maxItems": 16, "items": { "$ref": "#/components/schemas/PendingApprovalSubject" } },
                "analysis_status": { "type": "string", "maxLength": 2048 },
                "analysis_reason_codes": { "type": "array", "maxItems": 8, "items": { "type": "string", "enum": ["unknown_program", "dynamic_command", "unsupported_syntax", "invalid_syntax", "analysis_limit_exceeded", "unresolved_path", "unresolved_executable", "unproven_containment"] } },
                "analysis_reasons": { "type": "array", "maxItems": 8, "items": { "type": "string", "maxLength": 2048 } },
                "containment": { "type": "array", "maxItems": 8, "items": { "type": "string", "maxLength": 2048 } },
                "decision_reasons": { "type": "array", "maxItems": 8, "items": { "type": "string", "maxLength": 2048 } },
                "safe_summary_title": { "type": "string", "maxLength": 2048 },
                "safe_summary_detail": { "type": "string", "maxLength": 2048 },
                "operation": { "type": ["string", "null"], "maxLength": 2048 },
                "risk": { "type": ["string", "null"], "maxLength": 2048 },
                "snapshot_required": { "type": "boolean" },
                "command_family_allow_pattern": { "type": ["string", "null"], "maxLength": 2048 }
            }
        }),
    );
    schemas.insert(
        "PendingApprovalSubject".to_owned(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["kind", "scope"],
            "properties": {
                "kind": { "type": "string", "maxLength": 2048 },
                "scope": { "type": "string", "maxLength": 2048 },
                "workspace_label": { "type": ["string", "null"], "maxLength": 2048 }
            }
        }),
    );
    schemas.insert(
        "PublicRunEvent".to_owned(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["schema_version", "session_id", "run_id", "sequence", "event"],
            "properties": {
                "schema_version": { "type": "integer", "const": sigil_kernel::PUBLIC_RUN_EVENT_SCHEMA_VERSION },
                "session_id": { "type": "string", "maxLength": 512 },
                "run_id": { "type": "string", "maxLength": 512 },
                "sequence": { "type": "integer", "format": "uint64", "minimum": 1 },
                "event": { "$ref": "#/components/schemas/PublicRunEventPayload" }
            }
        }),
    );
    let event_variants = public_event_variants();
    schemas.insert(
        "PublicRunEventPayload".to_owned(),
        json!({
            "oneOf": event_variants
                .iter()
                .map(|(name, _)| name)
                .map(|name| json!({ "$ref": format!("#/components/schemas/{name}") }))
                .collect::<Vec<_>>()
        }),
    );
    schemas.insert(
        "PublicTaskPhase".to_owned(),
        json!({
            "type": "string",
            "enum": ["routing", "planning", "execution", "integration", "synthesis", "terminal"]
        }),
    );
    schemas.insert(
        "PublicSessionRouteTransitionView".to_owned(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["kind", "connection_id", "model_id", "remote_context_reset"],
            "properties": {
                "kind": { "type": "string", "enum": ["exact", "rebound", "explicitly_confirmed"] },
                "connection_id": { "type": ["string", "null"] },
                "model_id": { "type": ["string", "null"] },
                "remote_context_reset": { "type": "boolean" }
            }
        }),
    );
    schemas.insert(
        "PublicTaskPlanStep".to_owned(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["step_id", "title", "role", "depends_on", "mode", "isolation"],
            "properties": {
                "step_id": { "type": "string", "maxLength": 512 },
                "title": { "type": "string", "maxLength": 131072 },
                "role": { "type": "string", "maxLength": 512 },
                "depends_on": { "type": "array", "items": { "type": "string", "maxLength": 512 } },
                "mode": { "type": "string", "maxLength": 512 },
                "isolation": { "type": "string", "maxLength": 512 }
            }
        }),
    );
    schemas.insert(
        "PublicToolCall".to_owned(),
        json!({
            "type": "object",
            "required": ["id", "name", "args_json"],
            "properties": {
                "id": { "type": "string" },
                "name": { "type": "string" },
                "args_json": { "type": "string" }
            }
        }),
    );
    schemas.insert(
        "PublicToolPreview".to_owned(),
        json!({
            "type": "object",
            "required": ["title", "summary", "body", "changed_files", "file_diffs"],
            "properties": {
                "title": { "type": "string" },
                "summary": { "type": "string" },
                "body": { "type": "string" },
                "changed_files": { "type": "array", "items": { "type": "string" } },
                "file_diffs": { "type": "array", "items": { "type": "object" } }
            }
        }),
    );
    schemas.insert(
        "PublicToolResult".to_owned(),
        json!({
            "type": "object",
            "required": ["call_id", "tool_name", "content", "status", "metadata"],
            "properties": {
                "call_id": { "type": "string" },
                "tool_name": { "type": "string" },
                "content": { "type": "string" },
                "status": {
                    "oneOf": [
                        { "type": "string", "const": "ok" },
                        {
                            "type": "object",
                            "required": ["error"],
                            "properties": { "error": { "type": "object" } }
                        }
                    ]
                },
                "metadata": { "type": "object" }
            }
        }),
    );
    schemas.insert(
        "PublicToolProgress".to_owned(),
        json!({
            "type": "object",
            "required": ["execution_id", "call_id", "tool_name", "sequence", "status", "details"],
            "properties": {
                "execution_id": { "type": "string" },
                "call_id": { "type": "string" },
                "tool_name": { "type": "string" },
                "sequence": { "type": "integer", "format": "uint64" },
                "status": { "type": "string" },
                "message": { "type": "string" },
                "output_preview": { "type": "string" },
                "output_log_ref": { "type": "string" },
                "total_bytes": { "type": "integer", "format": "uint64" },
                "updated_at_ms": { "type": "integer", "format": "uint64" },
                "details": {}
            }
        }),
    );
    schemas.insert(
        "PublicAssistantMessage".to_owned(),
        json!({
            "type": "object",
            "required": ["id", "content", "tool_calls"],
            "properties": {
                "id": { "type": "string" },
                "content": { "type": ["string", "null"] },
                "tool_calls": { "type": "array", "items": { "$ref": "#/components/schemas/PublicToolCall" } },
                "assistant_kind": { "type": "string", "enum": ["tool_preamble", "progress", "reasoning_trace", "final_answer"] }
            }
        }),
    );

    for (name, schema) in event_variants {
        schemas.insert(name.to_owned(), schema);
    }
    schemas
}

fn public_event_variants() -> Vec<(&'static str, Value)> {
    vec![
        (
            "RouteTransitionEvent",
            public_event_variant(
                "route_transition",
                &["transition"],
                json_properties(json!({
                    "transition": { "$ref": "#/components/schemas/PublicSessionRouteTransitionView" }
                })),
                true,
            ),
        ),
        (
            "RunStartedEvent",
            public_event_variant(
                "run_started",
                &["prompt"],
                json_properties(json!({ "prompt": { "type": "string" } })),
                true,
            ),
        ),
        (
            "TaskRunStartedEvent",
            public_event_variant(
                "task_run_started",
                &["task_id", "objective"],
                json_properties(json!({
                    "task_id": { "type": "string", "maxLength": 512 },
                    "objective": { "type": "string", "maxLength": 131072 }
                })),
                true,
            ),
        ),
        (
            "RunFinishedEvent",
            public_event_variant(
                "run_finished",
                &["final_text"],
                json_properties(json!({ "final_text": { "type": "string" } })),
                true,
            ),
        ),
        (
            "RunAwaitingUserInputEvent",
            public_event_variant(
                "run_awaiting_user_input",
                &["request_id", "generation", "request_hash"],
                json_properties(json!({
                    "request_id": { "type": "string", "maxLength": 512 },
                    "generation": { "type": "integer", "format": "uint32", "minimum": 1 },
                    "request_hash": { "$ref": "#/components/schemas/Sha256" }
                })),
                true,
            ),
        ),
        (
            "TaskRunFinishedEvent",
            public_event_variant(
                "task_run_finished",
                &["task_id", "status"],
                json_properties(json!({
                    "task_id": { "type": "string", "maxLength": 512 },
                    "status": { "type": "string", "maxLength": 512 }
                })),
                true,
            ),
        ),
        (
            "TaskRoutingChangedEvent",
            public_event_variant(
                "task_routing_changed",
                &["handoff_id", "status", "task_id"],
                json_properties(json!({
                    "handoff_id": { "type": "string", "maxLength": 512 },
                    "status": { "type": "string", "maxLength": 512 },
                    "task_id": { "type": ["string", "null"], "maxLength": 512 }
                })),
                true,
            ),
        ),
        (
            "ConversationRouteChangedEvent",
            public_event_variant(
                "conversation_route_changed",
                &["decision_id", "route", "status"],
                json_properties(json!({
                    "decision_id": { "type": "string", "maxLength": 512 },
                    "route": { "type": "string", "enum": ["chat", "plan_review", "task"] },
                    "status": { "type": "string", "maxLength": 512 }
                })),
                true,
            ),
        ),
        (
            "PlanReviewChangedEvent",
            public_event_variant(
                "plan_review_changed",
                &["plan_review_id", "plan_id", "status"],
                json_properties(json!({
                    "plan_review_id": { "type": "string", "maxLength": 512 },
                    "plan_id": { "type": "string", "maxLength": 512 },
                    "status": { "type": "string", "enum": ["started", "waiting_for_input", "finalizing", "draft_ready", "completed_without_draft", "failed", "interrupted", "cancelled"] }
                })),
                true,
            ),
        ),
        (
            "UserInputChangedEvent",
            public_event_variant(
                "user_input_changed",
                &[
                    "request_id",
                    "generation",
                    "request_hash",
                    "status",
                    "request",
                ],
                json_properties(json!({
                    "request_id": { "type": "string", "maxLength": 512 },
                    "generation": { "type": "integer", "format": "uint32", "minimum": 1 },
                    "request_hash": { "$ref": "#/components/schemas/Sha256" },
                    "status": { "type": "string", "enum": ["requested", "decision_accepted", "continuation_claimed", "continuation_started", "resolved"] },
                    "request": { "$ref": "#/components/schemas/UserInputRequest" }
                })),
                true,
            ),
        ),
        (
            "TaskPhaseChangedEvent",
            public_event_variant(
                "task_phase_changed",
                &["task_id", "phase", "status"],
                json_properties(json!({
                    "task_id": { "type": ["string", "null"], "maxLength": 512 },
                    "phase": { "$ref": "#/components/schemas/PublicTaskPhase" },
                    "status": { "type": "string", "maxLength": 512 }
                })),
                true,
            ),
        ),
        (
            "TaskPlanUpdatedEvent",
            public_event_variant(
                "task_plan_updated",
                &["task_id", "plan_version", "status", "steps"],
                json_properties(json!({
                    "task_id": { "type": "string", "maxLength": 512 },
                    "plan_version": { "type": "integer", "format": "uint32" },
                    "status": { "type": "string", "maxLength": 512 },
                    "steps": { "type": "array", "items": { "$ref": "#/components/schemas/PublicTaskPlanStep" } }
                })),
                true,
            ),
        ),
        (
            "TaskBatchChangedEvent",
            public_event_variant(
                "task_batch_changed",
                &[
                    "task_id",
                    "plan_version",
                    "batch_id",
                    "active",
                    "completed",
                    "failed",
                ],
                json_properties(json!({
                    "task_id": { "type": "string", "maxLength": 512 },
                    "plan_version": { "type": "integer", "format": "uint32" },
                    "batch_id": { "type": "string", "maxLength": 512 },
                    "active": { "type": "integer", "format": "uint32" },
                    "completed": { "type": "integer", "format": "uint32" },
                    "failed": { "type": "integer", "format": "uint32" }
                })),
                true,
            ),
        ),
        (
            "TaskStepChangedEvent",
            public_event_variant(
                "task_step_changed",
                &["task_id", "plan_version", "step_id", "attempt_id", "status"],
                json_properties(json!({
                    "task_id": { "type": "string", "maxLength": 512 },
                    "plan_version": { "type": "integer", "format": "uint32" },
                    "step_id": { "type": "string", "maxLength": 512 },
                    "attempt_id": { "type": ["string", "null"], "maxLength": 512 },
                    "status": { "type": "string", "maxLength": 512 }
                })),
                true,
            ),
        ),
        (
            "IntegrationLaneChangedEvent",
            public_event_variant(
                "integration_lane_changed",
                &[
                    "task_id",
                    "plan_version",
                    "plan_id",
                    "lane_id",
                    "status",
                    "conflicts",
                ],
                json_properties(json!({
                    "task_id": { "type": "string", "maxLength": 512 },
                    "plan_version": { "type": "integer", "format": "uint32" },
                    "plan_id": { "type": "string", "maxLength": 512 },
                    "lane_id": { "type": "string", "maxLength": 512 },
                    "status": { "type": "string", "maxLength": 512 },
                    "conflicts": { "type": "array", "items": { "type": "string" } }
                })),
                true,
            ),
        ),
        (
            "RunFailedEvent",
            public_event_variant(
                "run_failed",
                &["error"],
                json_properties(json!({ "error": { "type": "string" } })),
                true,
            ),
        ),
        (
            "RouteRecoveryRequiredEvent",
            public_event_variant(
                "route_recovery_required",
                &["code", "actions", "recovery_binding", "retryable"],
                json_properties(json!({
                    "code": {
                        "type": "string",
                        "enum": ["session_route_confirmation_required", "session_route_selection_required", "model_route_not_configured", "connection_config_invalid", "provider_unavailable", "session_already_active", "session_writer_busy", "session_stream_invalid"]
                    },
                    "actions": {
                        "type": "array",
                        "uniqueItems": true,
                        "items": {
                            "type": "string",
                            "enum": ["confirm_current_route", "repair_connection", "select_replacement", "start_new_session", "retry_provider", "retry_session_attach", "back_to_session_library"]
                        }
                    },
                    "recovery_binding": { "type": "string", "maxLength": 128 },
                    "retryable": { "type": "boolean" }
                })),
                true,
            ),
        ),
        (
            "RunCancelledEvent",
            public_event_variant("run_cancelled", &[], Map::new(), true),
        ),
        (
            "TextDeltaEvent",
            public_event_variant(
                "text_delta",
                &["text"],
                json_properties(json!({ "text": { "type": "string" } })),
                true,
            ),
        ),
        (
            "ReasoningDeltaEvent",
            public_event_variant(
                "reasoning_delta",
                &["text"],
                json_properties(json!({ "text": { "type": "string" } })),
                true,
            ),
        ),
        (
            "ToolCallStartedEvent",
            public_event_variant(
                "tool_call_started",
                &["call"],
                json_properties(
                    json!({ "call": { "$ref": "#/components/schemas/PublicToolCall" } }),
                ),
                false,
            ),
        ),
        (
            "ToolCallArgsDeltaEvent",
            public_event_variant(
                "tool_call_args_delta",
                &["id", "delta"],
                json_properties(json!({
                    "id": { "type": "string" },
                    "delta": { "type": "string" }
                })),
                true,
            ),
        ),
        (
            "ToolCallCompletedEvent",
            public_event_variant(
                "tool_call_completed",
                &["call"],
                json_properties(
                    json!({ "call": { "$ref": "#/components/schemas/PublicToolCall" } }),
                ),
                false,
            ),
        ),
        (
            "ApprovalRequestedEvent",
            public_event_variant(
                "approval_requested",
                &[
                    "call",
                    "session_grant_available",
                    "session_grant_unavailable_reason",
                    "snapshot_required",
                ],
                json_properties(json!({
                    "call": { "$ref": "#/components/schemas/PublicToolCall" },
                    "session_grant_available": { "type": "boolean" },
                    "session_grant_unavailable_reason": {
                        "oneOf": [
                            { "$ref": "#/components/schemas/SessionGrantUnavailableReason" },
                            { "type": "null" }
                        ]
                    },
                    "spec": { "type": "object" },
                    "subjects": { "type": "array", "items": { "type": "object" } },
                    "network_effect": { "type": "string" },
                    "local_policy_decision": { "type": "string" },
                    "network_policy_decision": { "type": "string" },
                    "source_policy_decision": { "type": "string" },
                    "operation": { "type": "string" },
                    "risk": { "type": "string" },
                    "subject_zones": { "type": "array", "items": { "type": "string" } },
                    "confirmation": { "type": "object" },
                    "snapshot_required": { "type": "boolean" },
                    "command_permission_matches": { "type": "array", "items": { "type": "object" } },
                    "preview": {
                        "oneOf": [
                            { "$ref": "#/components/schemas/PublicToolPreview" },
                            { "type": "null" }
                        ]
                    }
                })),
                false,
            ),
        ),
        (
            "ApprovalResolvedEvent",
            public_event_variant(
                "approval_resolved",
                &["call_id", "approved", "reason"],
                json_properties(json!({
                    "call_id": { "type": "string" },
                    "approved": { "type": "boolean" },
                    "reason": { "type": ["string", "null"] }
                })),
                true,
            ),
        ),
        (
            "ToolResultEvent",
            public_event_variant(
                "tool_result",
                &["result"],
                json_properties(
                    json!({ "result": { "$ref": "#/components/schemas/PublicToolResult" } }),
                ),
                false,
            ),
        ),
        (
            "ToolProgressEvent",
            public_event_variant(
                "tool_progress",
                &["progress"],
                json_properties(
                    json!({ "progress": { "$ref": "#/components/schemas/PublicToolProgress" } }),
                ),
                false,
            ),
        ),
        (
            "TerminalLifecycleEvent",
            public_event_variant(
                "terminal_lifecycle",
                &["event"],
                json_properties(json!({
                    "event": { "$ref": "#/components/schemas/TerminalLifecycle" }
                })),
                true,
            ),
        ),
        (
            "UsageEvent",
            public_event_variant(
                "usage",
                &["usage"],
                json_properties(json!({ "usage": { "type": "object" } })),
                false,
            ),
        ),
        (
            "ContinuationStateEvent",
            public_event_variant(
                "continuation_state",
                &["state"],
                json_properties(json!({ "state": { "type": "object" } })),
                false,
            ),
        ),
        (
            "ControlEvent",
            public_event_variant(
                "control",
                &["control"],
                json_properties(json!({
                    "control": {
                        "type": "object",
                        "required": ["kind"],
                        "properties": {
                            "kind": { "type": "string" },
                            "payload": {}
                        }
                    }
                })),
                false,
            ),
        ),
        (
            "AssistantMessageEvent",
            public_event_variant(
                "assistant_message",
                &["message"],
                json_properties(json!({
                    "message": { "$ref": "#/components/schemas/PublicAssistantMessage" }
                })),
                false,
            ),
        ),
        (
            "NoticeEvent",
            public_event_variant(
                "notice",
                &["message"],
                json_properties(json!({ "message": { "type": "string" } })),
                true,
            ),
        ),
    ]
}

fn public_event_variant(
    event_type: &str,
    required_fields: &[&str],
    mut properties: Map<String, Value>,
    exact: bool,
) -> Value {
    properties.insert(
        "type".to_owned(),
        json!({ "type": "string", "const": event_type }),
    );
    let mut required = vec![Value::String("type".to_owned())];
    required.extend(
        required_fields
            .iter()
            .map(|field| Value::String((*field).to_owned())),
    );
    json!({
        "type": "object",
        "additionalProperties": !exact,
        "required": required,
        "properties": properties
    })
}

fn json_properties(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .cloned()
        .expect("event properties must be an object")
}
