//! Desktop-owned launcher and client boundary.
//!
//! The crate owns the local `sigil serve` child, per-launch bearer, bootstrap handshake, and
//! shutdown lifecycle. It deliberately does not depend on the kernel, runtime, TUI, or HTTP server
//! implementation. A future native shell can depend on this crate without exposing process or
//! credential primitives to its renderer.

mod client;
mod dto;
mod events;
mod launcher;
mod manager;
mod protocol;
mod secret;

pub use client::{DesktopClientError, DesktopHttpClient, DesktopRunEventStream};
pub use dto::{
    DesktopApprovalCommandReceipt, DesktopApprovalDecision, DesktopApprovalDecisionRecord,
    DesktopApprovalDecisionRequest, DesktopApprovalRecordedDecision, DesktopCatalogQuery,
    DesktopCommandEnvelope, DesktopPendingApproval, DesktopRunApprovalMode,
    DesktopRunCancelCommandReceipt, DesktopRunCancelRequest, DesktopRunSnapshot,
    DesktopRunStartCommandReceipt, DesktopRunStartRequest, DesktopRunStatus,
    DesktopSessionCatalogEntry, DesktopSessionCatalogPage, DesktopSessionCatalogState,
    DesktopSessionCreateRequest, DesktopSessionListResponse, DesktopSessionOpenRequest,
    DesktopSessionSnapshot, DesktopVerificationAction, DesktopVerificationCheckStatus,
    DesktopVerificationEvidence, DesktopVerificationRecommendationKind,
    DesktopVerificationRerunCommandReceipt, DesktopVerificationRerunRequest,
    DesktopVerificationScope, DesktopVerificationVerdict, DesktopVerificationView,
};
pub use events::{
    DESKTOP_PROTOCOL_EVENT_SCHEMA_VERSION, DESKTOP_PUBLIC_RUN_EVENT_SCHEMA_VERSION,
    DesktopProtocolEvent, DesktopProtocolEventClass, DesktopProtocolEventError,
    DesktopPublicRunEvent, DesktopTimelineApproval, DesktopTimelineEvent, DesktopTimelineEventKind,
};
pub use launcher::{
    DesktopLaunchError, DesktopLaunchRequest, DesktopLauncher, DesktopServerProcess,
    DesktopShutdownError, DesktopShutdownKind, DesktopShutdownReport,
};
pub use manager::{
    DesktopConnectionState, DesktopWorkspaceManager, DesktopWorkspaceManagerError,
    DesktopWorkspaceOpenRequest, DesktopWorkspaceSummary,
};
pub use protocol::{DesktopServerAuthentication, DesktopServerCapabilities, DesktopServerInfo};
