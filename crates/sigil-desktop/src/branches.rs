//! Path-free catalog references and exact selected conclusions for branch navigation.
use serde::{Deserialize, Serialize};

/// A related conversation, identified by its catalog reference and durable identity.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DesktopBranchLink {
    pub session_ref: String,
    pub session_id: String,
    pub title: Option<String>,
    pub source_turn_index: usize,
    pub source_turn_digest: String,
}
/// Parent and direct children visible to the current workspace catalog.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DesktopBranchLineage {
    pub session_id: String,
    pub parent: Option<DesktopBranchLink>,
    pub children: Vec<DesktopBranchLink>,
    pub unavailable_count: usize,
}
/// Exact source catalog entry selected for a conclusion preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DesktopBranchKnowledgeSource {
    pub source_session_ref: String,
    pub source_session_id: String,
}
/// One completed assistant conclusion and the immutable identities binding its preview.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DesktopBranchKnowledgePoint {
    pub source_turn_digest: String,
    pub source_message_id: String,
    pub source_text_sha256: String,
    pub summary_sha256: String,
    pub summary: String,
    pub truncated: bool,
}
/// Bounded conclusions from the exact selected source conversation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DesktopBranchKnowledgePreview {
    pub source_session_ref: String,
    pub source_session_id: String,
    pub points: Vec<DesktopBranchKnowledgePoint>,
}
/// The six immutable source bindings; the host resolves the actual summary text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DesktopBranchKnowledgeImport {
    pub source_session_ref: String,
    pub source_session_id: String,
    pub source_turn_digest: String,
    pub source_message_id: String,
    pub source_text_sha256: String,
    pub summary_sha256: String,
}
/// Durable import acknowledgement; an exact repeated import has the same identity.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DesktopBranchKnowledgeReceipt {
    pub import_id: String,
    pub already_imported: bool,
}
