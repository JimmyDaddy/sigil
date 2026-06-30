mod auth;
mod config;
mod driver;
mod dto;
mod listener;
mod openapi;
mod protocol;
mod registry;
mod sse;

pub use auth::{HttpAuthError, HttpAuthValidator};
pub use config::{DEFAULT_HTTP_TOKEN_ENV, HttpAuthConfig, HttpServerConfig, HttpServerConfigError};
pub use driver::{
    HttpRunDriver, HttpRunDriverApproval, HttpRunDriverCancel, HttpRunDriverError,
    HttpRunDriverStart,
};
pub use dto::{
    HttpApprovalCommandReceipt, HttpApprovalDecision, HttpApprovalDecisionRecord,
    HttpApprovalDecisionRequest, HttpPendingApproval, HttpRunApprovalMode, HttpRunSnapshot,
    HttpRunStartCommandReceipt, HttpRunStartRequest, HttpRunStatus, HttpSessionCreateRequest,
    HttpSessionSnapshot,
};
pub use listener::{HttpListenerError, HttpLocalServer};
pub use openapi::{HTTP_OPENAPI_VERSION, http_openapi_document};
pub use protocol::{HTTP_PROTOCOL_VERSION, HttpCommandEnvelope, HttpProtocolVersionError};
pub use registry::{HttpRegistryError, HttpSessionRunRegistry};
pub use sse::{
    HTTP_PROTOCOL_EVENT_SCHEMA_VERSION, HTTP_RUN_EVENT_SSE_NAME, HttpDurableEventView,
    HttpLiveEventBus, HttpLiveEventRecvError, HttpLiveEventSubscriber, HttpProtocolCursor,
    HttpProtocolCursorError, HttpProtocolEvent, HttpProtocolEventBuffer, HttpProtocolEventClass,
    HttpProtocolEventView, HttpProtocolReplayError, HttpRunEventSequencer, HttpSseError,
    HttpSseEvent, HttpTransientEventView, public_run_event_to_sse,
};

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
