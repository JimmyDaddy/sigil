/// Supplies a real approved artifact for tests about execution, limits or recovery. The fixture
/// has an explicit original deliverable, so those tests need no additional semantic admission.
pub(crate) fn append(
    session: &mut sigil_kernel::Session,
    task_id: &sigil_kernel::TaskId,
    objective: &str,
) -> anyhow::Result<sigil_kernel::TaskDirectExecutionAdmittedV1> {
    let source = serde_json::json!({"summary":"Complete the fixture objective","steps":[{
        "step_id":"deliver","title":"Deliver the requested result","role":"executor","depends_on":[],
        "mode":"read","isolation":"shared_read_only","deliverables":[objective]
    }],"target_paths":[]});
    let draft = sigil_kernel::plan_draft_created_entry(
        &format!("```sigil-plan-v2\n{source}\n```"),
        sigil_kernel::PlanSourceRef::default(),
        1,
        None,
    )?
    .expect("typed fixture draft");
    let admission = sigil_kernel::TaskDirectExecutionAdmittedV1::approved_plan(
        task_id.clone(),
        objective,
        draft.plan_id.clone(),
        draft.plan_hash.clone(),
        1,
    );
    session.append_control(sigil_kernel::ControlEntry::PlanDraftCreated(draft))?;
    Ok(admission)
}
