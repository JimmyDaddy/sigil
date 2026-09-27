use super::*;

impl DesktopHttpClient {
    /// Reads related conversations without opening or executing them.
    pub async fn branch_lineage(
        &self,
        session_id: &str,
    ) -> Result<crate::DesktopBranchLineage, DesktopClientError> {
        validate_stream_identity(session_id)?;
        let view = self
            .get_json(
                self.route(["sessions", session_id, "branches"])?,
                StatusCode::OK,
            )
            .await?;
        Ok(view)
    }

    /// Previews completed conclusions from an exact catalog source, without importing them.
    pub async fn branch_knowledge_preview(
        &self,
        session_id: &str,
        source: crate::DesktopBranchKnowledgeSource,
    ) -> Result<crate::DesktopBranchKnowledgePreview, DesktopClientError> {
        validate_stream_identity(session_id)?;
        validate_recovery_token(&source.source_session_ref)?;
        validate_recovery_token(&source.source_session_id)?;
        let preview: crate::DesktopBranchKnowledgePreview = self
            .post_json(
                self.route(["sessions", session_id, "branches", "knowledge-preview"])?,
                &source,
                StatusCode::OK,
            )
            .await?;
        if preview.source_session_ref != source.source_session_ref
            || preview.source_session_id != source.source_session_id
        {
            return Err(DesktopClientError::InvalidResponse);
        }
        Ok(preview)
    }
}
