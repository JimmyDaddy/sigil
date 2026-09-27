//! Narrow camel-case branch projections and exact selection inputs.
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopBranchLink {
    pub(crate) session_ref: String,
    pub(crate) session_id: String,
    pub(crate) title: Option<String>,
    pub(crate) source_turn_index: usize,
    pub(crate) source_turn_digest: String,
}

impl From<sigil_desktop::DesktopBranchLink> for DesktopBranchLink {
    fn from(value: sigil_desktop::DesktopBranchLink) -> Self {
        Self {
            session_ref: value.session_ref,
            session_id: value.session_id,
            title: value.title,
            source_turn_index: value.source_turn_index,
            source_turn_digest: value.source_turn_digest,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopBranchLineage {
    pub(crate) session_id: String,
    pub(crate) parent: Option<DesktopBranchLink>,
    pub(crate) children: Vec<DesktopBranchLink>,
    pub(crate) unavailable_count: usize,
}

impl From<sigil_desktop::DesktopBranchLineage> for DesktopBranchLineage {
    fn from(value: sigil_desktop::DesktopBranchLineage) -> Self {
        Self {
            session_id: value.session_id,
            parent: value.parent.map(Into::into),
            children: value.children.into_iter().map(Into::into).collect(),
            unavailable_count: value.unavailable_count,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopBranchKnowledgeSource {
    pub(crate) source_session_ref: String,
    pub(crate) source_session_id: String,
}

impl From<DesktopBranchKnowledgeSource> for sigil_desktop::DesktopBranchKnowledgeSource {
    fn from(value: DesktopBranchKnowledgeSource) -> Self {
        Self {
            source_session_ref: value.source_session_ref,
            source_session_id: value.source_session_id,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopBranchKnowledgePoint {
    pub(crate) source_turn_digest: String,
    pub(crate) source_message_id: String,
    pub(crate) source_text_sha256: String,
    pub(crate) summary_sha256: String,
    pub(crate) summary: String,
    pub(crate) truncated: bool,
}

impl From<sigil_desktop::DesktopBranchKnowledgePoint> for DesktopBranchKnowledgePoint {
    fn from(value: sigil_desktop::DesktopBranchKnowledgePoint) -> Self {
        Self {
            source_turn_digest: value.source_turn_digest,
            source_message_id: value.source_message_id,
            source_text_sha256: value.source_text_sha256,
            summary_sha256: value.summary_sha256,
            summary: value.summary,
            truncated: value.truncated,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopBranchKnowledgePreview {
    pub(crate) source_session_ref: String,
    pub(crate) source_session_id: String,
    pub(crate) points: Vec<DesktopBranchKnowledgePoint>,
}

impl From<sigil_desktop::DesktopBranchKnowledgePreview> for DesktopBranchKnowledgePreview {
    fn from(value: sigil_desktop::DesktopBranchKnowledgePreview) -> Self {
        Self {
            source_session_ref: value.source_session_ref,
            source_session_id: value.source_session_id,
            points: value.points.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopBranchKnowledgeImport {
    pub(crate) source_session_ref: String,
    pub(crate) source_session_id: String,
    pub(crate) source_turn_digest: String,
    pub(crate) source_message_id: String,
    pub(crate) source_text_sha256: String,
    pub(crate) summary_sha256: String,
}

impl From<DesktopBranchKnowledgeImport> for sigil_desktop::DesktopBranchKnowledgeImport {
    fn from(value: DesktopBranchKnowledgeImport) -> Self {
        Self {
            source_session_ref: value.source_session_ref,
            source_session_id: value.source_session_id,
            source_turn_digest: value.source_turn_digest,
            source_message_id: value.source_message_id,
            source_text_sha256: value.source_text_sha256,
            summary_sha256: value.summary_sha256,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopBranchKnowledgeReceipt {
    pub(crate) import_id: String,
    pub(crate) already_imported: bool,
}

impl From<sigil_desktop::DesktopBranchKnowledgeReceipt> for DesktopBranchKnowledgeReceipt {
    fn from(value: sigil_desktop::DesktopBranchKnowledgeReceipt) -> Self {
        Self {
            import_id: value.import_id,
            already_imported: value.already_imported,
        }
    }
}
