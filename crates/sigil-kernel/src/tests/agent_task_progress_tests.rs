use super::*;

pub(super) fn admit_direct_fixture(
    session: &mut Session,
    task_id: &str,
    objective: &str,
) -> Result<crate::TaskDirectExecutionAdmittedV1> {
    let task_id = TaskId::new(task_id)?;
    let admission =
        crate::TaskDirectExecutionAdmittedV1::task_request(task_id.clone(), objective, 1);
    session.append_controls(vec![
        ControlEntry::TaskRun(TaskRunEntry {
            task_id,
            parent_session_ref: SessionRef::new_relative("fixture.jsonl")?,
            objective: objective.to_owned(),
            title: None,
            status: TaskRunStatus::Started,
            reason: None,
        }),
        ControlEntry::TaskDirectExecutionAdmittedV1(admission.clone()),
    ])?;
    Ok(admission)
}

struct ProgressProvider {
    inner: ScriptedTurnToolProvider,
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait]
impl Provider for ProgressProvider {
    fn name(&self) -> &str {
        "task-progress-fixture"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.requests
            .lock()
            .expect("captured requests")
            .push(request.clone());
        self.inner.stream(request).await
    }
}

fn progress_agent(
    turns: Vec<Vec<(String, String, String)>>,
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
) -> Agent<ProgressProvider> {
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    Agent::new(
        ProgressProvider {
            inner: ScriptedTurnToolProvider::new(turns),
            requests,
        },
        tools,
    )
}

fn assert_plain_task_requests(requests: &[CompletionRequest]) {
    for request in requests {
        assert!(request.tools.iter().any(|tool| tool.name == "echo"));
        assert!(!request.tools.iter().any(|tool| matches!(
            tool.name.as_str(),
            "bind_direct_task_requirements" | "task_completion_claim"
        )));
        assert!(
            !request
                .messages
                .iter()
                .filter_map(|message| message.content.as_deref())
                .any(|text| {
                    text.contains("Task completion claim binding")
                        || text.contains("Completion declaration correction only")
                        || text
                            .contains("Cover the original source text with exact UTF-8 byte spans")
                })
        );
    }
}

#[tokio::test]
async fn direct_task_uses_tools_then_finishes_without_claims() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut session = Session::new("task-progress-fixture", "model");
    let task_id = TaskId::new("plain-task")?;
    let objective = "检查中文输入与 emoji 🦀，报告结果";
    let admission = admit_direct_fixture(&mut session, task_id.as_str(), objective)?;
    let purpose = AgentRunPurpose::TaskDirectExecution(crate::TaskDirectExecutionContext {
        task_id,
        admission_id: admission.admission_id,
        attempt_id: "direct-attempt".to_owned(),
    });
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = progress_agent(
        vec![vec![(
            "actual-read".to_owned(),
            "echo".to_owned(),
            json!({"value":objective}).to_string(),
        )]],
        Arc::clone(&requests),
    );
    let mut options = scripted_run_options(3);
    options.workspace_root = temp.path().to_path_buf();
    let output = agent
        .run_with_input(
            &mut session,
            AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(objective)])
                .with_run_purpose(purpose),
            options,
            &mut crate::event::NoopEventHandler,
        )
        .await?;
    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    assert!(output.result.final_message_id.is_some());
    assert_eq!(output.result.tool_calls, 1);
    assert_eq!(
        output.outcome.terminal_reason,
        AgentRunTerminalReason::FinalAnswer
    );
    assert!(session.entries().iter().any(|entry| matches!(entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(event))
            if event.call_id == "actual-read" && event.status == ToolExecutionStatus::Completed
    )));
    let requests = requests.lock().expect("captured requests");
    assert_eq!(requests.len(), 2, "the Task uses one normal tool loop");
    assert_plain_task_requests(&requests);
    Ok(())
}

#[tokio::test]
async fn task_progress_updates_and_reopens_without_requirement_admission() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("progress.jsonl");
    let mut session = Session::load_from_store(
        "task-progress-fixture",
        "model",
        JsonlSessionStore::new(&path)?,
    )?;
    let task_id = TaskId::new("progress-task")?;
    let admission = admit_direct_fixture(&mut session, task_id.as_str(), "Inspect and summarize")?;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let checklist =
        |status| json!({"items":[{"text":"Inspect source", "status":status}]}).to_string();
    let agent = progress_agent(
        vec![
            vec![
                (
                    "progress-start".to_owned(),
                    crate::UPDATE_TASK_CHECKLIST_TOOL_NAME.to_owned(),
                    checklist("in_progress"),
                ),
                (
                    "read".to_owned(),
                    "echo".to_owned(),
                    json!({"value":"source evidence"}).to_string(),
                ),
            ],
            vec![(
                "progress-done".to_owned(),
                crate::UPDATE_TASK_CHECKLIST_TOOL_NAME.to_owned(),
                checklist("completed"),
            )],
        ],
        Arc::clone(&requests),
    );
    let mut options = scripted_run_options(4);
    options.workspace_root = temp.path().to_path_buf();
    let output = agent
        .run_with_input(
            &mut session,
            AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
                "Inspect and summarize",
            )])
            .with_task_checklist_update(crate::TaskChecklistUpdateContextV1 {
                task_id: task_id.clone(),
                current_revision: 0,
            })
            .with_run_purpose(AgentRunPurpose::TaskDirectExecution(
                crate::TaskDirectExecutionContext {
                    task_id: task_id.clone(),
                    admission_id: admission.admission_id,
                    attempt_id: "progress-attempt".to_owned(),
                },
            )),
            options,
            &mut crate::event::NoopEventHandler,
        )
        .await?;
    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    assert!(!session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskDirectRequirementsBoundV1(_))
    )));
    drop(session);
    let session = Session::load_from_store(
        "task-progress-fixture",
        "model",
        JsonlSessionStore::new(&path)?,
    )?;
    let projection = session.task_state_projection();
    let progress = projection.tasks[&task_id]
        .checklist
        .as_ref()
        .expect("durable progress");
    assert_eq!(progress.revision, 2);
    assert_eq!(progress.items[0].text, "Inspect source");
    assert_eq!(
        progress.items[0].status,
        crate::TaskChecklistItemStatusV1::Completed
    );
    assert_plain_task_requests(&requests.lock().expect("captured requests"));
    Ok(())
}

#[tokio::test]
async fn malformed_checklist_does_not_lock_business_tools_or_final_answer() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut session = Session::new("task-progress-fixture", "model");
    let task_id = TaskId::new("bad-progress-task")?;
    let admission = admit_direct_fixture(&mut session, task_id.as_str(), "Inspect source")?;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = progress_agent(
        vec![
            vec![(
                "bad-progress".to_owned(),
                crate::UPDATE_TASK_CHECKLIST_TOOL_NAME.to_owned(),
                json!({"items":[{"text":"Inspect", "status":"partial"}]}).to_string(),
            )],
            vec![(
                "actual-read".to_owned(),
                "echo".to_owned(),
                json!({"value":"source evidence"}).to_string(),
            )],
        ],
        Arc::clone(&requests),
    );
    let mut options = scripted_run_options(4);
    options.workspace_root = temp.path().to_path_buf();
    let output = agent
        .run_with_input(
            &mut session,
            AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
                "Inspect source",
            )])
            .with_task_checklist_update(crate::TaskChecklistUpdateContextV1 {
                task_id: task_id.clone(),
                current_revision: 0,
            })
            .with_run_purpose(AgentRunPurpose::TaskDirectExecution(
                crate::TaskDirectExecutionContext {
                    task_id,
                    admission_id: admission.admission_id,
                    attempt_id: "bad-progress-attempt".to_owned(),
                },
            )),
            options,
            &mut crate::event::NoopEventHandler,
        )
        .await?;
    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    assert_eq!(
        output.outcome.terminal_reason,
        AgentRunTerminalReason::FinalAnswer
    );
    assert!(session.entries().iter().any(|entry| matches!(entry,
        SessionLogEntry::ToolResultV3(result)
            if result.call_id == "bad-progress" && result.facts.status == "error"
                && result.initial_model_view.preview.contains("pending")
                && result.initial_model_view.preview.contains("completed")
    )));
    assert!(session.entries().iter().any(|entry| matches!(entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(event))
            if event.call_id == "actual-read" && event.status == ToolExecutionStatus::Completed
    )));
    let requests = requests.lock().expect("captured requests");
    assert_eq!(requests.len(), 3);
    assert_plain_task_requests(&requests);
    Ok(())
}
