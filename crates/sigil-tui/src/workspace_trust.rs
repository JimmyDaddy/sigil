#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceTrustGateState {
    pub(crate) workspace_id: String,
}

impl WorkspaceTrustGateState {
    pub(crate) fn new(workspace_id: impl Into<String>) -> Self {
        Self {
            workspace_id: workspace_id.into(),
        }
    }
}
