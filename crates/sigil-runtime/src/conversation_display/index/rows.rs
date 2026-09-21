use super::*;

#[derive(Debug, Default)]
pub(super) struct RowProjectionState {
    active_run: Option<ActiveRunProjection>,
    tools: ToolProjectionState,
    approvals: HashMap<String, String>,
    skills: HashMap<String, ConversationDisplaySkillReferenceV1>,
    pub terminal_frontier: Option<ConversationTerminalFrontierV1>,
}

/// Only identities consumed by this exact source record, never a transcript or tool body.
#[derive(Debug, Clone)]
pub(super) struct RowSourceContext {
    active_run: Option<ActiveRunProjection>,
    tool: Option<(String, ToolProjection)>,
    execution: Option<(String, ToolProjection)>,
    terminal: Option<(String, command::CommandTerminalState)>,
    approval: Option<(String, String)>,
    skill: Option<(String, ConversationDisplaySkillReferenceV1)>,
    pub(super) tool_source: Option<ConversationDisplayRecordPosition>,
}

impl RowProjectionState {
    pub(super) fn context(&self, record: &SessionStreamRecord) -> Result<RowSourceContext> {
        let (tool, approval) = match record.session_log_entry()? {
            Some(SessionLogEntry::ToolResultV3(result)) => (
                self.tools
                    .items
                    .get(&ToolProjectionKey::Call(result.call_id.clone()))
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
        let execution_id = match record.session_log_entry()? {
            Some(SessionLogEntry::ToolResultV3(result)) => {
                command::execution_id_from_result(&result)
            }
            Some(SessionLogEntry::Control(ControlEntry::TerminalTask(task))) => {
                Some(task.handle.task_id.as_str().to_owned())
            }
            _ => None,
        };
        let terminal = execution_id.as_ref().and_then(|id| {
            self.tools
                .terminals
                .get(id)
                .cloned()
                .map(|state| (id.clone(), state))
        });
        let execution = execution_id.and_then(|id| {
            self.tools
                .items
                .get(&ToolProjectionKey::Execution(id.clone()))
                .cloned()
                .map(|tool| (id, tool))
        });
        Ok(RowSourceContext {
            execution,
            terminal,
            active_run: self.active_run.clone(),
            tool,
            tool_source: None,
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
        let projected = project_record(
            record,
            scope,
            &mut self.active_run,
            &mut self.tools,
            &mut self.approvals,
            &mut self.skills,
            &mut self.terminal_frontier,
        )?;
        if let Some(SessionLogEntry::Assistant(message)) = record.session_log_entry()? {
            for call in message.tool_calls {
                if let Some(tool) = self.tools.items.get_mut(&ToolProjectionKey::Call(call.id)) {
                    tool.input = None;
                }
            }
        }
        if let Some(SessionLogEntry::ToolResultV3(result)) = record.session_log_entry()?
            && let Some(id) = command::execution_id_from_result(&result)
            && let Some(tool) = self.tools.items.get_mut(&ToolProjectionKey::Execution(id))
        {
            tool.input = None;
        }
        Ok(projected)
    }
}

impl RowSourceContext {
    pub(super) fn tool_input_source_sequence(&self) -> Option<u64> {
        self.execution
            .as_ref()
            .or(self.tool.as_ref())
            .filter(|(_, tool)| tool.input_json_bytes.is_some())
            .map(|(_, tool)| tool.requested_sequence)
    }

    pub(super) fn hydrated_input_bytes(&self) -> usize {
        self.execution
            .as_ref()
            .or(self.tool.as_ref())
            .and_then(|(_, tool)| tool.input_json_bytes)
            .map_or(0, |bytes| bytes + r#","input":"#.len())
    }

    pub(super) fn project(
        &self,
        record: &SessionStreamRecord,
        scope: &str,
        records: &BodyRecords<'_>,
    ) -> Result<Vec<ConversationDisplayItemV1>> {
        let mut tool = self.tool.clone();
        let mut execution = self.execution.clone();
        if let Some(position) = &self.tool_source {
            let source = records.record(position)?;
            let Some(SessionLogEntry::Assistant(message)) = source.session_log_entry()? else {
                bail!("command input source is not an assistant tool call");
            };
            if let Some((_, tool)) = execution.as_mut().or(tool.as_mut()) {
                let call = message
                    .tool_calls
                    .iter()
                    .find(|call| call.id == tool.requested_call_id)
                    .context("command input source lost its exact tool call")?;
                tool.input = command_input(call);
                let input_bytes = tool
                    .input
                    .as_ref()
                    .map(serde_json::to_vec)
                    .transpose()?
                    .map(|bytes| bytes.len());
                if input_bytes != tool.input_json_bytes {
                    bail!("command input source changed its bounded projection");
                }
            }
        }
        let mut state = RowProjectionState {
            active_run: self.active_run.clone(),
            tools: ToolProjectionState {
                items: tool
                    .into_iter()
                    .map(|(id, tool)| (ToolProjectionKey::Call(id), tool))
                    .chain(
                        execution
                            .into_iter()
                            .map(|(id, tool)| (ToolProjectionKey::Execution(id), tool)),
                    )
                    .collect(),
                terminals: self.terminal.clone().into_iter().collect(),
            },
            approvals: self.approval.clone().into_iter().collect(),
            skills: self.skill.clone().into_iter().collect(),
            terminal_frontier: None,
        };
        state.apply(record, scope)
    }
}
