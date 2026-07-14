use super::*;

pub(super) fn apply_verification_projection_record(
    projection: &mut VerificationStateProjection,
    cursor: &mut Option<ProjectionCursor>,
    record: &SessionStreamRecord,
) -> Result<()> {
    let next_cursor = record.projection_cursor(VERIFICATION_STATE_PROJECTION_SCHEMA_VERSION);
    match projection_apply_decision_for_record(
        cursor.as_ref(),
        &next_cursor.session_id,
        next_cursor.last_applied_stream_sequence,
        &next_cursor.last_applied_event_id,
        &next_cursor.last_applied_record_checksum,
    )? {
        ProjectionApplyDecision::IgnoreAlreadyApplied => return Ok(()),
        ProjectionApplyDecision::Apply => {}
    }
    if let Some(domain_record) = record.domain_event_record()?
        && let Some(SessionLogEntry::Control(control)) =
            session_entry_from_domain_event(&domain_record.event)?
    {
        projection.apply_control_entry(&control);
    }
    *cursor = Some(next_cursor);
    Ok(())
}

pub(super) fn apply_plan_approval_projection_record(
    projection: &mut PlanApprovalProjection,
    cursor: &mut Option<ProjectionCursor>,
    record: &SessionStreamRecord,
) -> Result<()> {
    let next_cursor = record.projection_cursor(PLAN_APPROVAL_PROJECTION_SCHEMA_VERSION);
    match projection_apply_decision_for_record(
        cursor.as_ref(),
        &next_cursor.session_id,
        next_cursor.last_applied_stream_sequence,
        &next_cursor.last_applied_event_id,
        &next_cursor.last_applied_record_checksum,
    )? {
        ProjectionApplyDecision::IgnoreAlreadyApplied => return Ok(()),
        ProjectionApplyDecision::Apply => {}
    }
    if let Some(domain_record) = record.domain_event_record()?
        && let Some(SessionLogEntry::Control(control)) =
            session_entry_from_domain_event(&domain_record.event)?
    {
        projection.apply_control_entry(&control);
    }
    *cursor = Some(next_cursor);
    Ok(())
}

pub(super) fn apply_plan_artifact_projection_record(
    projection: &mut PlanArtifactProjection,
    cursor: &mut Option<ProjectionCursor>,
    record: &SessionStreamRecord,
) -> Result<()> {
    let next_cursor = record.projection_cursor(PLAN_ARTIFACT_PROJECTION_SCHEMA_VERSION);
    match projection_apply_decision_for_record(
        cursor.as_ref(),
        &next_cursor.session_id,
        next_cursor.last_applied_stream_sequence,
        &next_cursor.last_applied_event_id,
        &next_cursor.last_applied_record_checksum,
    )? {
        ProjectionApplyDecision::IgnoreAlreadyApplied => return Ok(()),
        ProjectionApplyDecision::Apply => {}
    }
    if let Some(domain_record) = record.domain_event_record()?
        && let Some(SessionLogEntry::Control(control)) =
            session_entry_from_domain_event(&domain_record.event)?
    {
        projection.apply_control_entry(&control);
    }
    *cursor = Some(next_cursor);
    Ok(())
}

pub(super) fn apply_task_projection_record(
    projection: &mut TaskStateProjection,
    cursor: &mut Option<ProjectionCursor>,
    record: &SessionStreamRecord,
) -> Result<()> {
    let next_cursor = record.projection_cursor(TASK_STATE_PROJECTION_SCHEMA_VERSION);
    match projection_apply_decision_for_record(
        cursor.as_ref(),
        &next_cursor.session_id,
        next_cursor.last_applied_stream_sequence,
        &next_cursor.last_applied_event_id,
        &next_cursor.last_applied_record_checksum,
    )? {
        ProjectionApplyDecision::IgnoreAlreadyApplied => return Ok(()),
        ProjectionApplyDecision::Apply => {}
    }
    if let Some(domain_record) = record.domain_event_record()?
        && let Some(SessionLogEntry::Control(control)) =
            session_entry_from_domain_event(&domain_record.event)?
    {
        projection.apply_control_entry(&control);
    }
    *cursor = Some(next_cursor);
    Ok(())
}

pub(super) fn apply_skill_projection_record(
    projection: &mut SkillStateProjection,
    cursor: &mut Option<ProjectionCursor>,
    record: &SessionStreamRecord,
) -> Result<()> {
    let next_cursor = record.projection_cursor(SKILL_STATE_PROJECTION_SCHEMA_VERSION);
    match projection_apply_decision_for_record(
        cursor.as_ref(),
        &next_cursor.session_id,
        next_cursor.last_applied_stream_sequence,
        &next_cursor.last_applied_event_id,
        &next_cursor.last_applied_record_checksum,
    )? {
        ProjectionApplyDecision::IgnoreAlreadyApplied => return Ok(()),
        ProjectionApplyDecision::Apply => {}
    }
    if let Some(domain_record) = record.domain_event_record()?
        && let Some(SessionLogEntry::Control(control)) =
            session_entry_from_domain_event(&domain_record.event)?
    {
        projection.apply_control_entry(&control);
    }
    *cursor = Some(next_cursor);
    Ok(())
}

pub(super) fn apply_plugin_projection_record(
    projection: &mut PluginStateProjection,
    cursor: &mut Option<ProjectionCursor>,
    record: &SessionStreamRecord,
) -> Result<()> {
    let next_cursor = record.projection_cursor(PLUGIN_STATE_PROJECTION_SCHEMA_VERSION);
    match projection_apply_decision_for_record(
        cursor.as_ref(),
        &next_cursor.session_id,
        next_cursor.last_applied_stream_sequence,
        &next_cursor.last_applied_event_id,
        &next_cursor.last_applied_record_checksum,
    )? {
        ProjectionApplyDecision::IgnoreAlreadyApplied => return Ok(()),
        ProjectionApplyDecision::Apply => {}
    }
    if let Some(domain_record) = record.domain_event_record()?
        && let Some(SessionLogEntry::Control(control)) =
            session_entry_from_domain_event(&domain_record.event)?
    {
        projection.apply_control_entry(&control);
    }
    *cursor = Some(next_cursor);
    Ok(())
}

pub(super) fn apply_agent_thread_projection_record(
    projection: &mut AgentThreadStateProjection,
    cursor: &mut Option<ProjectionCursor>,
    record: &SessionStreamRecord,
) -> Result<()> {
    let next_cursor = record.projection_cursor(AGENT_THREAD_STATE_PROJECTION_SCHEMA_VERSION);
    match projection_apply_decision_for_record(
        cursor.as_ref(),
        &next_cursor.session_id,
        next_cursor.last_applied_stream_sequence,
        &next_cursor.last_applied_event_id,
        &next_cursor.last_applied_record_checksum,
    )? {
        ProjectionApplyDecision::IgnoreAlreadyApplied => return Ok(()),
        ProjectionApplyDecision::Apply => {}
    }
    if let Some(domain_record) = record.domain_event_record()?
        && let Some(SessionLogEntry::Control(control)) =
            session_entry_from_domain_event(&domain_record.event)?
    {
        projection.apply_control_entry(&control);
    }
    *cursor = Some(next_cursor);
    Ok(())
}

pub(super) fn apply_agent_profile_trust_projection_record(
    projection: &mut AgentProfileTrustProjection,
    cursor: &mut Option<ProjectionCursor>,
    record: &SessionStreamRecord,
) -> Result<()> {
    let next_cursor = record.projection_cursor(AGENT_PROFILE_TRUST_PROJECTION_SCHEMA_VERSION);
    match projection_apply_decision_for_record(
        cursor.as_ref(),
        &next_cursor.session_id,
        next_cursor.last_applied_stream_sequence,
        &next_cursor.last_applied_event_id,
        &next_cursor.last_applied_record_checksum,
    )? {
        ProjectionApplyDecision::IgnoreAlreadyApplied => return Ok(()),
        ProjectionApplyDecision::Apply => {}
    }
    if let Some(domain_record) = record.domain_event_record()?
        && let Some(SessionLogEntry::Control(control)) =
            session_entry_from_domain_event(&domain_record.event)?
    {
        projection.apply_control_entry(&control);
    }
    *cursor = Some(next_cursor);
    Ok(())
}

pub(super) fn apply_agent_profile_policy_projection_record(
    projection: &mut AgentProfilePolicyProjection,
    cursor: &mut Option<ProjectionCursor>,
    record: &SessionStreamRecord,
) -> Result<()> {
    let next_cursor = record.projection_cursor(AGENT_PROFILE_POLICY_PROJECTION_SCHEMA_VERSION);
    match projection_apply_decision_for_record(
        cursor.as_ref(),
        &next_cursor.session_id,
        next_cursor.last_applied_stream_sequence,
        &next_cursor.last_applied_event_id,
        &next_cursor.last_applied_record_checksum,
    )? {
        ProjectionApplyDecision::IgnoreAlreadyApplied => return Ok(()),
        ProjectionApplyDecision::Apply => {}
    }
    if let Some(domain_record) = record.domain_event_record()?
        && let Some(SessionLogEntry::Control(control)) =
            session_entry_from_domain_event(&domain_record.event)?
    {
        projection.apply_control_entry(&control);
    }
    *cursor = Some(next_cursor);
    Ok(())
}

pub(super) fn apply_agent_result_continuation_projection_record(
    projection: &mut AgentResultContinuationProjection,
    cursor: &mut Option<ProjectionCursor>,
    record: &SessionStreamRecord,
) -> Result<()> {
    let next_cursor = record.projection_cursor(AGENT_RESULT_CONTINUATION_PROJECTION_SCHEMA_VERSION);
    match projection_apply_decision_for_record(
        cursor.as_ref(),
        &next_cursor.session_id,
        next_cursor.last_applied_stream_sequence,
        &next_cursor.last_applied_event_id,
        &next_cursor.last_applied_record_checksum,
    )? {
        ProjectionApplyDecision::IgnoreAlreadyApplied => return Ok(()),
        ProjectionApplyDecision::Apply => {}
    }
    if let Some(domain_record) = record.domain_event_record()?
        && let Some(SessionLogEntry::Control(control)) =
            session_entry_from_domain_event(&domain_record.event)?
    {
        projection.apply_control_entry(&control);
    }
    *cursor = Some(next_cursor);
    Ok(())
}

#[cfg(test)]
pub(super) fn apply_conversation_queue_projection_record(
    projection: &mut ConversationQueueProjection,
    cursor: &mut Option<ProjectionCursor>,
    record: &SessionStreamRecord,
) -> Result<()> {
    let next_cursor = record.projection_cursor(CONVERSATION_QUEUE_PROJECTION_SCHEMA_VERSION);
    match projection_apply_decision_for_record(
        cursor.as_ref(),
        &next_cursor.session_id,
        next_cursor.last_applied_stream_sequence,
        &next_cursor.last_applied_event_id,
        &next_cursor.last_applied_record_checksum,
    )? {
        ProjectionApplyDecision::IgnoreAlreadyApplied => return Ok(()),
        ProjectionApplyDecision::Apply => {}
    }
    if let Some(domain_record) = record.domain_event_record()?
        && let Some(SessionLogEntry::Control(control)) =
            session_entry_from_domain_event(&domain_record.event)?
    {
        projection.apply_control_entry(&control);
    }
    *cursor = Some(next_cursor);
    Ok(())
}

pub(super) fn apply_changeset_projection_record(
    projection: &mut ChangeSetProjection,
    cursor: &mut Option<ProjectionCursor>,
    record: &SessionStreamRecord,
) -> Result<()> {
    let next_cursor = record.projection_cursor(CHANGESET_PROJECTION_SCHEMA_VERSION);
    match projection_apply_decision_for_record(
        cursor.as_ref(),
        &next_cursor.session_id,
        next_cursor.last_applied_stream_sequence,
        &next_cursor.last_applied_event_id,
        &next_cursor.last_applied_record_checksum,
    )? {
        ProjectionApplyDecision::IgnoreAlreadyApplied => return Ok(()),
        ProjectionApplyDecision::Apply => {}
    }
    if let Some(domain_record) = record.domain_event_record()?
        && let Some(SessionLogEntry::Control(control)) =
            session_entry_from_domain_event(&domain_record.event)?
    {
        projection.apply_control_entry(&control);
    }
    *cursor = Some(next_cursor);
    Ok(())
}

pub(super) fn apply_write_isolation_projection_record(
    projection: &mut WriteIsolationProjection,
    cursor: &mut Option<ProjectionCursor>,
    record: &SessionStreamRecord,
) -> Result<()> {
    let next_cursor = record.projection_cursor(WRITE_ISOLATION_PROJECTION_SCHEMA_VERSION);
    match projection_apply_decision_for_record(
        cursor.as_ref(),
        &next_cursor.session_id,
        next_cursor.last_applied_stream_sequence,
        &next_cursor.last_applied_event_id,
        &next_cursor.last_applied_record_checksum,
    )? {
        ProjectionApplyDecision::IgnoreAlreadyApplied => return Ok(()),
        ProjectionApplyDecision::Apply => {}
    }
    if let Some(domain_record) = record.domain_event_record()?
        && let Some(SessionLogEntry::Control(control)) =
            session_entry_from_domain_event(&domain_record.event)?
    {
        projection.apply_control_entry(&control);
    }
    *cursor = Some(next_cursor);
    Ok(())
}

pub(super) fn apply_terminal_task_projection_record(
    projection: &mut TerminalTaskProjection,
    cursor: &mut Option<ProjectionCursor>,
    record: &SessionStreamRecord,
) -> Result<()> {
    let next_cursor = record.projection_cursor(TERMINAL_TASK_PROJECTION_SCHEMA_VERSION);
    match projection_apply_decision_for_record(
        cursor.as_ref(),
        &next_cursor.session_id,
        next_cursor.last_applied_stream_sequence,
        &next_cursor.last_applied_event_id,
        &next_cursor.last_applied_record_checksum,
    )? {
        ProjectionApplyDecision::IgnoreAlreadyApplied => return Ok(()),
        ProjectionApplyDecision::Apply => {}
    }
    if let Some(domain_record) = record.domain_event_record()?
        && let Some(SessionLogEntry::Control(control)) =
            session_entry_from_domain_event(&domain_record.event)?
    {
        projection.apply_control_entry(&control);
    }
    *cursor = Some(next_cursor);
    Ok(())
}

pub(super) fn apply_usage_projection_record(
    stats: &mut SessionStats,
    cursor: &mut Option<ProjectionCursor>,
    record: &SessionStreamRecord,
) -> Result<()> {
    let next_cursor = record.projection_cursor(USAGE_STATE_PROJECTION_SCHEMA_VERSION);
    match projection_apply_decision_for_record(
        cursor.as_ref(),
        &next_cursor.session_id,
        next_cursor.last_applied_stream_sequence,
        &next_cursor.last_applied_event_id,
        &next_cursor.last_applied_record_checksum,
    )? {
        ProjectionApplyDecision::IgnoreAlreadyApplied => return Ok(()),
        ProjectionApplyDecision::Apply => {}
    }
    if let Some(domain_record) = record.domain_event_record()?
        && let Some(SessionLogEntry::Control(control)) =
            session_entry_from_domain_event(&domain_record.event)?
    {
        apply_usage_control_entry(stats, &control);
    }
    *cursor = Some(next_cursor);
    Ok(())
}
