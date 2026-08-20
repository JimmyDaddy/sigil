use std::path::Path;

use anyhow::Result;
use serde_json::json;

use crate::{
    ControlEntry, PlanApprovalExpiry, PlanApprovalScope, PlanPermissionGrantedEntry, Session,
    SessionLogEntry, TOOL_APPROVAL_SESSION_GRANT_SCHEMA_VERSION, ToolApprovalSessionGrantEntry,
    ToolApprovalSessionGrantExpiry, ToolPermissionPlanV2, ToolSubjectAudit,
    permission::{
        ApprovalMode, InteractionMode, PermissionDecision, PermissionDecisionReason,
        PermissionDecisionSource, PermissionRisk, ToolApprovalSessionGrantFacet,
        ToolApprovalSessionGrantScope, tool_approval_session_grant_available_for_plan,
        tool_approval_session_grant_shape,
    },
    tool::{ToolSpec, ToolSubject, ToolSubjectKind, ToolSubjectScope},
};

use super::AgentRunOptions;
use super::tool_audit::stable_json_hash;

pub(super) type PlanApprovalAuthority = PlanPermissionGrantedEntry;

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
        return Some(grant);
    }
    None
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
    plan: &ToolPermissionPlanV2,
    policy_version: &str,
    mut decision: PermissionDecision,
) -> (PermissionDecision, Option<ToolApprovalSessionGrantEntry>) {
    if decision.mode != ApprovalMode::Ask
        || !tool_approval_session_grant_available_for_plan(&decision, plan)
    {
        return (decision, None);
    }
    let matching_grant = session.entries().iter().rev().find_map(|entry| {
        let SessionLogEntry::Control(ControlEntry::ToolApprovalSessionGrant(grant)) = entry else {
            return None;
        };
        session_grant_covers_decision(grant, plan, policy_version, &decision).then(|| grant.clone())
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
        decision.reasons.push(PermissionDecisionReason {
            source: PermissionDecisionSource::SessionGrant,
            code: "session_grant_v2_match".to_owned(),
            detail: format!("bounded session grant {} matched", grant.grant_id),
        });
        decision.recompute_mode();
    }
    (decision, matching_grant)
}

pub(super) fn session_grant_covers_decision(
    grant: &ToolApprovalSessionGrantEntry,
    plan: &ToolPermissionPlanV2,
    policy_version: &str,
    decision: &PermissionDecision,
) -> bool {
    let Some(shape) = tool_approval_session_grant_shape(decision) else {
        return false;
    };
    let Some(containment_binding) = plan.session_grant_containment_binding() else {
        return false;
    };
    grant.schema_version == TOOL_APPROVAL_SESSION_GRANT_SCHEMA_VERSION
        && grant.expires == ToolApprovalSessionGrantExpiry::Session
        && grant.tool_name == plan.tool_name
        && plan.analysis.is_complete()
        && !plan.has_non_grantable_effect()
        && plan.semantic_scope.as_ref() == Some(&grant.semantic_scope)
        && plan.effects.is_subset(&grant.effect_ceiling)
        && decision.risk <= grant.risk_ceiling
        && grant.facets == shape.facets
        && grant.scope == shape.scope
        && grant.containment_binding == containment_binding
        && (grant.policy_version == policy_version
            || session_grant_policy_fingerprint(decision)
                .is_ok_and(|fingerprint| fingerprint == grant.policy_version))
        && match &grant.scope {
            ToolApprovalSessionGrantScope::ExactSubjects => {
                grant_subjects_match_decision(&grant.subjects, &decision.subjects)
            }
            ToolApprovalSessionGrantScope::NetworkReadTool => {
                !grant.subjects.is_empty()
                    && grant.subjects.iter().all(|subject| {
                        subject.kind == ToolSubjectKind::NetworkEndpoint
                            && !subject.identity_sha256.trim().is_empty()
                    })
                    && decision.subjects.iter().all(|subject| {
                        subject.kind == ToolSubjectKind::NetworkEndpoint
                            && !subject.normalized.trim().is_empty()
                    })
            }
            ToolApprovalSessionGrantScope::CommandFamily { prefix } => decision
                .subjects
                .iter()
                .any(|subject| command_subject_matches_family_prefix(subject, prefix.as_str())),
        }
}

/// A `CommandFamily` grant covers an unknown-family command subject whose whitespace-normalized
/// command shares the grant's first-two-token prefix on a token boundary. Known `family:`
/// subjects are never matched here; they keep the exact-subject path.
fn command_subject_matches_family_prefix(subject: &ToolSubject, prefix: &str) -> bool {
    subject.kind == ToolSubjectKind::Command
        && !subject.normalized.starts_with("family:")
        && crate::permission::command_family_prefix_matches(prefix, &subject.original)
}

pub(super) fn session_grant_policy_fingerprint(decision: &PermissionDecision) -> Result<String> {
    // A CommandFamily grant must be stable across argument variants: the command subject is
    // projected to its derived first-two-token prefix so `python3 script.py` and
    // `python3 script.py --flag` share one policy version.
    let family_prefix =
        tool_approval_session_grant_shape(decision).and_then(|shape| match shape.scope {
            ToolApprovalSessionGrantScope::CommandFamily { prefix } => Some(prefix),
            _ => None,
        });
    let subjects = decision
        .subjects
        .iter()
        .map(|subject| {
            let mut audit = ToolSubjectAudit::from(subject);
            if let Some(prefix) = family_prefix.as_ref()
                && subject.kind == ToolSubjectKind::Command
                && crate::permission::command_family_prefix(&subject.original).as_deref()
                    == Some(prefix.as_str())
            {
                audit.identity_sha256 = crate::stable_event_hash(prefix.as_bytes());
            }
            audit
        })
        .collect::<Vec<_>>();
    stable_json_hash(&json!({
        "schema_version": 1,
        "access": decision.access,
        "network_effect": decision.network_effect,
        "local_policy_decision": decision.local_policy_decision,
        "network_policy_decision": decision.network_policy_decision,
        "source_policy_decision": decision.source_policy_decision,
        "operation": decision.operation,
        "risk": decision.risk,
        "subjects": subjects,
        "subject_zones": decision.subject_zones,
        "external_directory_required": decision.external_directory_required,
        "confirmation": decision.confirmation,
        "snapshot_required": decision.snapshot_required,
    }))
    .map(|digest| format!("session-scope-sha256:{digest}"))
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
    let value = subject.identity_sha256.trim();
    if value.is_empty() {
        return None;
    }
    Some((
        subject.kind.as_str().to_owned(),
        subject.scope.as_str().to_owned(),
        value.to_owned(),
    ))
}

fn decision_subject_key(subject: &ToolSubject) -> Option<(String, String, String)> {
    let audited = ToolSubjectAudit::from(subject);
    let value = audited.identity_sha256;
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
            // RFC-0067: the adoption authority carries the plan-scoped grant.
            SessionLogEntry::Control(ControlEntry::PlanExecutionAdoptedV1(adoption))
                if adoption.permission_grant.is_some() =>
            {
                let candidate = &adoption.adopted_candidate;
                let scope = candidate.permission_scope_candidate.as_ref()?;
                Some((
                    index,
                    PlanPermissionGrantedEntry {
                        plan_id: adoption.plan_id.clone(),
                        plan_hash: adoption.plan_hash.clone(),
                        task_id: adoption.task_id.clone(),
                        workspace_snapshot_id: candidate
                            .compile_binding
                            .base_workspace_snapshot_id
                            .clone(),
                        permission: adoption.permission_grant?,
                        scope: PlanApprovalScope {
                            summary: scope.summary.clone(),
                            workspace_paths: scope.workspace_paths.clone(),
                        },
                        expires: PlanApprovalExpiry::Session,
                        granted_at_ms: adoption.adopted_at_ms,
                    },
                ))
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
