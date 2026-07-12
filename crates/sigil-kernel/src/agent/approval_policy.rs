use std::path::Path;

use crate::{
    ControlEntry, PlanApprovalExpiry, PlanApprovedEntry, PlanPermissionGrantedEntry, Session,
    SessionLogEntry, ToolApprovalSessionGrantEntry, ToolApprovalSessionGrantExpiry,
    ToolSubjectAudit,
    permission::{
        ApprovalMode, InteractionMode, PermissionDecision, PermissionRisk,
        ToolApprovalSessionGrantFacet, ToolApprovalSessionGrantScope,
        tool_approval_session_grant_shape,
    },
    tool::{ToolSpec, ToolSubject, ToolSubjectKind, ToolSubjectScope},
};

use super::AgentRunOptions;

#[derive(Debug, Clone)]
pub(super) enum PlanApprovalAuthority {
    PermissionGrant(PlanPermissionGrantedEntry),
    ApprovedPlan(crate::PlanApprovedEntry),
}

pub(super) fn plan_approval_decision_override(
    session: &Session,
    spec: &ToolSpec,
    mut decision: PermissionDecision,
) -> PermissionDecision {
    if decision.mode != ApprovalMode::Ask
        || decision.external_directory_required
        || decision.local_policy_decision != ApprovalMode::Ask
        || decision.network_policy_decision != ApprovalMode::Allow
        || decision.source_policy_decision != ApprovalMode::Allow
        || !plan_approval_can_auto_allow_decision(&decision)
    {
        return decision;
    }
    if active_plan_approval_authority(session, spec, &decision).is_some() {
        decision.local_policy_decision = ApprovalMode::Allow;
        decision.recompute_mode();
    }
    decision
}

pub(super) fn active_plan_approval_authority(
    session: &Session,
    spec: &ToolSpec,
    decision: &PermissionDecision,
) -> Option<PlanApprovalAuthority> {
    if decision.mode != ApprovalMode::Ask
        || decision.external_directory_required
        || decision.local_policy_decision != ApprovalMode::Ask
        || decision.network_policy_decision != ApprovalMode::Allow
        || decision.source_policy_decision != ApprovalMode::Allow
        || !plan_approval_can_auto_allow_decision(decision)
    {
        return None;
    }
    if let Some(grant) = active_plan_permission_grant(session)
        && grant.permission.covers_tool(spec)
        && plan_approval_covers_subjects(&grant.scope.workspace_paths, &decision.subjects)
    {
        return Some(PlanApprovalAuthority::PermissionGrant(grant));
    }
    let approval = active_plan_approval(session)?;
    if approval.permission.covers_tool(spec)
        && plan_approval_covers_subjects(&approval.scope.workspace_paths, &decision.subjects)
    {
        Some(PlanApprovalAuthority::ApprovedPlan(approval))
    } else {
        None
    }
}

pub(super) fn interactive_external_directory_approval_override(
    options: &AgentRunOptions,
    mut decision: PermissionDecision,
) -> PermissionDecision {
    if decision.external_directory_required
        && decision.mode == ApprovalMode::Deny
        && options.interaction_mode == InteractionMode::Interactive
    {
        decision.request_external_directory_interactive_approval();
    }
    decision
}

pub(super) fn tool_session_grant_decision_override(
    session: &Session,
    tool_name: &str,
    mut decision: PermissionDecision,
) -> (PermissionDecision, Option<ToolApprovalSessionGrantEntry>) {
    if decision.mode != ApprovalMode::Ask || tool_approval_session_grant_shape(&decision).is_none()
    {
        return (decision, None);
    }
    let matching_grant = session.entries().iter().rev().find_map(|entry| {
        let SessionLogEntry::Control(ControlEntry::ToolApprovalSessionGrant(grant)) = entry else {
            return None;
        };
        session_grant_covers_decision(grant, tool_name, &decision).then(|| grant.clone())
    });
    if let Some(grant) = matching_grant.as_ref() {
        for facet in &grant.facets {
            match facet {
                ToolApprovalSessionGrantFacet::Local => {
                    decision.local_policy_decision = ApprovalMode::Allow;
                }
                ToolApprovalSessionGrantFacet::Network => {
                    decision.network_policy_decision = ApprovalMode::Allow;
                }
            }
        }
        decision.recompute_mode();
    }
    (decision, matching_grant)
}

pub(super) fn session_grant_covers_decision(
    grant: &ToolApprovalSessionGrantEntry,
    tool_name: &str,
    decision: &PermissionDecision,
) -> bool {
    let Some(shape) = tool_approval_session_grant_shape(decision) else {
        return false;
    };
    grant.expires == ToolApprovalSessionGrantExpiry::Session
        && grant.tool_name == tool_name
        && grant.access == decision.access
        && grant.network_effect == decision.network_effect
        && grant.operation == decision.operation
        && grant.facets == shape.facets
        && grant.scope == shape.scope
        && match grant.scope {
            ToolApprovalSessionGrantScope::ExactSubjects => {
                grant_subjects_match_decision(&grant.subjects, &decision.subjects)
            }
            ToolApprovalSessionGrantScope::NetworkReadTool => {
                !grant.subjects.is_empty()
                    && grant.subjects.iter().all(|subject| {
                        subject.kind == ToolSubjectKind::NetworkEndpoint
                            && !subject.normalized.trim().is_empty()
                    })
                    && decision.subjects.iter().all(|subject| {
                        subject.kind == ToolSubjectKind::NetworkEndpoint
                            && !subject.normalized.trim().is_empty()
                    })
            }
        }
}

fn grant_subjects_match_decision(
    grant_subjects: &[ToolSubjectAudit],
    subjects: &[ToolSubject],
) -> bool {
    let mut left = grant_subjects
        .iter()
        .filter_map(grant_subject_key)
        .collect::<Vec<_>>();
    let mut right = subjects
        .iter()
        .filter_map(decision_subject_key)
        .collect::<Vec<_>>();
    if left.len() != grant_subjects.len() || right.len() != subjects.len() {
        return false;
    }
    left.sort();
    right.sort();
    left == right
}

fn grant_subject_key(subject: &ToolSubjectAudit) -> Option<(String, String, String)> {
    let value = if subject.kind == ToolSubjectKind::McpTrustClass {
        let original = subject.original.trim();
        (!original.is_empty()).then_some(original)?
    } else {
        match subject.scope {
            ToolSubjectScope::External => subject.canonical_path.as_deref().or_else(|| {
                let normalized = subject.normalized.trim();
                (!normalized.is_empty()).then_some(normalized)
            })?,
            ToolSubjectScope::Workspace | ToolSubjectScope::Unknown => {
                let normalized = subject.normalized.trim();
                (!normalized.is_empty()).then_some(normalized)?
            }
        }
    };
    Some((
        subject.kind.as_str().to_owned(),
        subject.scope.as_str().to_owned(),
        value.to_owned(),
    ))
}

fn decision_subject_key(subject: &ToolSubject) -> Option<(String, String, String)> {
    let value = if subject.kind == ToolSubjectKind::McpTrustClass {
        let original = subject.original.trim();
        (!original.is_empty()).then(|| original.to_owned())?
    } else {
        match subject.scope {
            ToolSubjectScope::External => subject
                .canonical_path
                .as_ref()
                .map(|path| path.display().to_string())
                .or_else(|| {
                    let normalized = subject.normalized.trim();
                    (!normalized.is_empty()).then(|| normalized.to_owned())
                })?,
            ToolSubjectScope::Workspace | ToolSubjectScope::Unknown => {
                let normalized = subject.normalized.trim();
                (!normalized.is_empty()).then(|| normalized.to_owned())?
            }
        }
    };
    Some((
        subject.kind.as_str().to_owned(),
        subject.scope.as_str().to_owned(),
        value,
    ))
}

fn plan_approval_can_auto_allow_decision(decision: &PermissionDecision) -> bool {
    matches!(decision.risk, PermissionRisk::Low | PermissionRisk::Medium)
}

fn active_plan_permission_grant(session: &Session) -> Option<PlanPermissionGrantedEntry> {
    let entries = session.entries();
    let (grant_index, grant) = entries
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, entry)| match entry {
            SessionLogEntry::Control(ControlEntry::PlanPermissionGranted(grant)) => {
                Some((index, grant.clone()))
            }
            _ => None,
        })?;
    if task_has_terminal_status_after(entries, &grant.task_id, grant_index) {
        return None;
    }
    match grant.expires {
        PlanApprovalExpiry::NextUserPrompt => {
            let user_messages_after_grant = entries
                .iter()
                .skip(grant_index.saturating_add(1))
                .filter(|entry| matches!(entry, SessionLogEntry::User(_)))
                .count();
            (user_messages_after_grant == 0).then_some(grant)
        }
        PlanApprovalExpiry::Session => Some(grant),
        PlanApprovalExpiry::AtUnixMs(expires_at_ms) => {
            (super::unix_time_ms() <= expires_at_ms).then_some(grant)
        }
    }
}

fn task_has_terminal_status_after(
    entries: &[SessionLogEntry],
    task_id: &crate::TaskId,
    start_index: usize,
) -> bool {
    entries
        .iter()
        .skip(start_index.saturating_add(1))
        .any(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskRun(run))
                    if &run.task_id == task_id && run.status.is_terminal()
            )
        })
}

pub(super) fn active_plan_approval(session: &Session) -> Option<PlanApprovedEntry> {
    let entries = session.entries();
    let (approval_index, approval) =
        entries
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, entry)| match entry {
                SessionLogEntry::Control(ControlEntry::PlanApproved(approval)) => {
                    Some((index, approval.clone()))
                }
                _ => None,
            })?;
    match approval.expires {
        PlanApprovalExpiry::NextUserPrompt => {
            let user_messages_after_approval = entries
                .iter()
                .skip(approval_index.saturating_add(1))
                .filter(|entry| matches!(entry, SessionLogEntry::User(_)))
                .count();
            (user_messages_after_approval == 1).then_some(approval)
        }
        PlanApprovalExpiry::Session => Some(approval),
        PlanApprovalExpiry::AtUnixMs(expires_at_ms) => {
            (super::unix_time_ms() <= expires_at_ms).then_some(approval)
        }
    }
}

fn plan_approval_covers_subjects(workspace_paths: &[String], subjects: &[ToolSubject]) -> bool {
    if subjects.is_empty() {
        return false;
    }
    subjects.iter().all(|subject| {
        subject.scope == ToolSubjectScope::Workspace
            && plan_approval_covers_subject(workspace_paths, subject)
    })
}

fn plan_approval_covers_subject(workspace_paths: &[String], subject: &ToolSubject) -> bool {
    // Empty scope means the accepted plan did not name a concrete workspace target. Keep the
    // write behind normal approval instead of widening an ambiguous plan to the full workspace.
    if workspace_paths.is_empty() {
        return false;
    }
    workspace_paths
        .iter()
        .any(|scope_path| path_is_within_scope(&subject.normalized, scope_path))
}

fn path_is_within_scope(path: &str, scope_path: &str) -> bool {
    let path_components = Path::new(path).components().collect::<Vec<_>>();
    let scope_components = Path::new(scope_path).components().collect::<Vec<_>>();
    !scope_components.is_empty()
        && path_components.len() >= scope_components.len()
        && path_components
            .iter()
            .zip(scope_components.iter())
            .all(|(left, right)| left == right)
}
