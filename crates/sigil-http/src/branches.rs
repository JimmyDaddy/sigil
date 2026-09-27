//! Narrow branch navigation and selected-knowledge transport values.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpBranchLink {
    pub session_ref: String,
    pub session_id: String,
    pub title: Option<String>,
    pub source_turn_index: usize,
    pub source_turn_digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpBranchLineage {
    pub session_id: String,
    pub parent: Option<HttpBranchLink>,
    pub children: Vec<HttpBranchLink>,
    pub unavailable_count: usize,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpBranchKnowledgeSource {
    pub source_session_ref: String,
    pub source_session_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpBranchKnowledgePoint {
    pub source_turn_digest: String,
    pub source_message_id: String,
    pub source_text_sha256: String,
    pub summary_sha256: String,
    pub summary: String,
    pub truncated: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpBranchKnowledgePreview {
    pub source_session_ref: String,
    pub source_session_id: String,
    pub points: Vec<HttpBranchKnowledgePoint>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpBranchKnowledgeImport {
    pub source_session_ref: String,
    pub source_session_id: String,
    pub source_turn_digest: String,
    pub source_message_id: String,
    pub source_text_sha256: String,
    pub summary_sha256: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpBranchKnowledgeReceipt {
    pub import_id: String,
    pub already_imported: bool,
}

impl From<sigil_runtime::application_branch_knowledge::ApplicationBranchLineageView>
    for HttpBranchLineage
{
    fn from(
        view: sigil_runtime::application_branch_knowledge::ApplicationBranchLineageView,
    ) -> Self {
        let link =
            |value: sigil_runtime::application_branch_knowledge::ApplicationBranchLinkView| {
                HttpBranchLink {
                    session_ref: value.session_ref.as_path().to_string_lossy().into_owned(),
                    session_id: value.session_id,
                    title: value.title,
                    source_turn_index: value.source_turn_index,
                    source_turn_digest: value.source_turn_digest,
                }
            };
        Self {
            session_id: view.session_id,
            parent: view.parent.map(link),
            children: view.children.into_iter().map(link).collect(),
            unavailable_count: view.unavailable_count,
        }
    }
}
impl From<sigil_runtime::application_branch_knowledge::ApplicationBranchKnowledgePreview>
    for HttpBranchKnowledgePreview
{
    fn from(
        view: sigil_runtime::application_branch_knowledge::ApplicationBranchKnowledgePreview,
    ) -> Self {
        Self {
            source_session_ref: view
                .source_session_ref
                .as_path()
                .to_string_lossy()
                .into_owned(),
            source_session_id: view.source_session_id,
            points: view
                .points
                .into_iter()
                .map(|point| HttpBranchKnowledgePoint {
                    source_turn_digest: point.source_turn_digest,
                    source_message_id: point.source_message_id,
                    source_text_sha256: point.source_text_sha256,
                    summary_sha256: point.summary_sha256,
                    summary: point.summary,
                    truncated: point.truncated,
                })
                .collect(),
        }
    }
}
