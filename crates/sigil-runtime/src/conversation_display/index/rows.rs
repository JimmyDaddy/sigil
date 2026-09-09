use super::*;

#[derive(Debug, Default)]
pub(super) struct RowProjectionState {
    active_run: Option<ActiveRunProjection>,
    tools: HashMap<String, ToolProjection>,
    approvals: HashMap<String, String>,
    skills: HashMap<String, ConversationDisplaySkillReferenceV1>,
    pub terminal_frontier: Option<ConversationTerminalFrontierV1>,
}

/// Only identities consumed by this exact source record, never a transcript or tool body.
#[derive(Debug, Clone)]
pub(super) struct RowSourceContext {
    active_run: Option<ActiveRunProjection>,
    tool: Option<(String, ToolProjection)>,
    approval: Option<(String, String)>,
    skill: Option<(String, ConversationDisplaySkillReferenceV1)>,
}

impl RowProjectionState {
    pub(super) fn context(&self, record: &SessionStreamRecord) -> Result<RowSourceContext> {
        let (tool, approval) = match record.session_log_entry()? {
            Some(SessionLogEntry::ToolResultV3(result)) => (
                self.tools
                    .get(&result.call_id)
                    .cloned()
                    .map(|tool| (result.call_id, tool)),
                None,
            ),
            Some(SessionLogEntry::Control(ControlEntry::ToolApproval(approval))) => (
                None,
                self.approvals
                    .get(&approval.call_id)
                    .cloned()
                    .map(|id| (approval.call_id, id)),
            ),
            _ => (None, None),
        };
        Ok(RowSourceContext {
            active_run: self.active_run.clone(),
            tool,
            approval,
            skill: self.active_run.as_ref().and_then(|run| {
                self.skills
                    .get(&run.run_id)
                    .cloned()
                    .map(|skill| (run.run_id.clone(), skill))
            }),
        })
    }

    pub(super) fn apply(
        &mut self,
        record: &SessionStreamRecord,
        scope: &str,
    ) -> Result<Vec<ConversationDisplayItemV1>> {
        project_record(
            record,
            scope,
            &mut self.active_run,
            &mut self.tools,
            &mut self.approvals,
            &mut self.skills,
            &mut self.terminal_frontier,
        )
    }
}

impl RowSourceContext {
    pub(super) fn project(
        &self,
        record: &SessionStreamRecord,
        scope: &str,
    ) -> Result<Vec<ConversationDisplayItemV1>> {
        let mut state = RowProjectionState {
            active_run: self.active_run.clone(),
            tools: self.tool.clone().into_iter().collect(),
            approvals: self.approval.clone().into_iter().collect(),
            skills: self.skill.clone().into_iter().collect(),
            terminal_frontier: None,
        };
        state.apply(record, scope)
    }
}
