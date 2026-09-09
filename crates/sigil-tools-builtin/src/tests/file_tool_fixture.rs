//! File-tool test host using the shipping authority, policy, durable audit and V3 seal.
//! The audit store lives outside the test workspace so read/search results and mutation-event
//! assertions observe only the fixture data they deliberately created.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sigil_kernel::permission_plan_v3::ToolPermissionPolicyFacetsV3;
use sigil_kernel::resource::{AuthorityGeneration, CanonicalHash};
use sigil_kernel::{
    ApprovalMode, ControlEntry, JsonlSessionStore, MutationEventRecorder, PermissionConfig,
    PermissionEvaluationContext, PermissionMode, PermissionPolicyChain, SessionLogEntry,
    TOOL_PERMISSION_DECISION_SCHEMA_VERSION, Tool, ToolCall, ToolContext,
    ToolPermissionDecisionV2Entry, ToolPermissionPlanDraft, ToolPermissionPlannedV2Entry,
    ToolPreview, ToolRegistry, ToolResult, ToolSubjectAudit,
};

use crate::file_tools::{
    DeleteFileTool, EditFileTool, GlobTool, GrepTool, ListTool, ReadFileTool, WriteFileTool,
};

pub(crate) fn file_authority_context(ctx: ToolContext) -> Result<ToolContext> {
    if ctx.tool_authority().is_some() {
        return Ok(ctx);
    }
    let workspace = ctx.workspace_root.canonicalize()?;
    let workspace_id = sigil_kernel::stable_workspace_id(&workspace)?;
    let generation = AuthorityGeneration {
        epoch: 1,
        instance_hash: sigil_resource_authority::identity::canonical_identity(&workspace)?.digest,
    };
    let registry = Arc::new(Mutex::new(
        sigil_resource_authority::borrowed::BorrowedSubjectRegistryV1::new(),
    ));
    registry
        .lock()
        .expect("fixture subject registry")
        .activate_workspace(
            "sigil",
            workspace_id.as_str().to_owned(),
            &workspace,
            generation,
        )?;
    let access = Arc::new(
        sigil_resource_authority::file_access::AuthorityManagedFileAccessServiceV1::new(registry),
    );
    let broker = Arc::new(sigil_kernel::capability_issuer::KernelCapabilityBrokerV1::new());
    Ok(ctx.with_tool_authority(Arc::new(
        sigil_kernel::tool_authority::KernelToolAuthorityV1::new(access, broker),
    )))
}

fn file_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    for tool in [
        Arc::new(ReadFileTool) as Arc<dyn Tool>,
        Arc::new(WriteFileTool),
        Arc::new(EditFileTool),
        Arc::new(DeleteFileTool),
        Arc::new(ListTool),
        Arc::new(GlobTool),
        Arc::new(GrepTool),
    ] {
        registry.register(tool);
    }
    registry
}

fn prepare_file_call(
    registry: &ToolRegistry,
    ctx: ToolContext,
    call: &ToolCall,
) -> Result<(ToolContext, tempfile::TempDir)> {
    let mut ctx = file_authority_context(ctx)?;
    let spec = registry
        .spec_for(&call.name)
        .context("registered file tool")?;
    let plan = registry.permission_plan(&ctx, call)?;
    let config = PermissionConfig {
        mode: PermissionMode::DangerFullAccess,
        ..Default::default()
    };
    let policy_context = PermissionEvaluationContext {
        workspace_root: ctx.workspace_root.clone(),
        ..Default::default()
    };
    let decision = PermissionPolicyChain::new_with_context(&config, &policy_context)
        .decide_plan(&spec, &plan)?;
    ensure!(
        decision.mode == ApprovalMode::Allow,
        "fixture policy did not allow {}: {:?}",
        call.name,
        decision.mode
    );
    ensure!(
        decision.confirmation.is_none(),
        "fixture cannot invent a confirmation"
    );
    let policy_version = format!(
        "file-fixture:{}",
        sigil_kernel::sha256_hex(&serde_json::to_vec(&config)?)
    );
    let audit_root = tempfile::tempdir()?;
    let audit_path = audit_root.path().join("authorization.jsonl");
    let audit_store = JsonlSessionStore::new(&audit_path)?;
    audit_store.append(&SessionLogEntry::Control(
        ControlEntry::ToolPermissionPlannedV2(Box::new(ToolPermissionPlannedV2Entry::from_plan(
            &call.id, &plan,
        )?)),
    ))?;
    audit_store.append(&SessionLogEntry::Control(
        ControlEntry::ToolPermissionDecisionV2(Box::new(ToolPermissionDecisionV2Entry {
            schema_version: TOOL_PERMISSION_DECISION_SCHEMA_VERSION,
            call_id: call.id.clone(),
            tool_name: call.name.clone(),
            plan_hash: plan.plan_hash.clone(),
            policy_version: policy_version.clone(),
            policy_decision: decision.mode,
            access: decision.access,
            network_effect: decision.network_effect,
            local_policy_decision: decision.local_policy_decision,
            network_policy_decision: decision.network_policy_decision,
            source_policy_decision: decision.source_policy_decision,
            operation: decision.operation,
            risk: decision.risk,
            subjects: decision
                .subjects
                .iter()
                .map(ToolSubjectAudit::from)
                .collect(),
            subject_zones: decision.subject_zones,
            external_directory_required: decision.external_directory_required,
            confirmation: decision.confirmation,
            snapshot_required: decision.snapshot_required,
            command_permission_matches: decision.command_permission_matches,
            decision_reasons: decision.reasons,
            allow_source: None,
            grant_id: None,
            prepared_digest: None,
        })),
    ))?;
    let entries = JsonlSessionStore::read_entries(&audit_path)?;
    let Some(SessionLogEntry::Control(ControlEntry::ToolPermissionDecisionV2(persisted))) =
        entries.last()
    else {
        anyhow::bail!("fixture authorization was not durably recorded");
    };
    let evidence =
        CanonicalHash::from_bytes(Sha256::digest(serde_json::to_vec(persisted.as_ref())?).into());
    let sealed_plan = sigil_kernel::permission_plan_v3_builder::v3_plan_from_v2(&plan);
    let sealed_decision = sigil_kernel::permission_plan_v3_builder::v3_decision_from_authorization(
        &sealed_plan,
        format!("decision:{}:policy", call.id),
        format!("policy:{}", call.id),
        evidence,
        call.id.clone(),
        policy_version,
        persisted.policy_decision,
        ToolPermissionPolicyFacetsV3 {
            external_directory_required: persisted.external_directory_required,
            session_grant_available: false,
            confirmation_required: false,
        },
        None,
        None,
        None,
    );
    if ctx.mutation_recorder.is_none() {
        ctx = ctx.with_mutation_recorder(MutationEventRecorder::new(audit_store));
    }
    Ok((
        ctx.with_prepared_permission_plan(plan)
            .with_v3_admission(Arc::new(sealed_plan), Some(Arc::new(sealed_decision))),
        audit_root,
    ))
}

/// Explicit fixture host boundary for direct file-tool tests. Production methods run unchanged.
#[async_trait]
pub(crate) trait FileToolTestExt: Tool {
    fn permission_plan_with_file_authority(
        &self,
        ctx: &ToolContext,
        args: &Value,
    ) -> Result<ToolPermissionPlanDraft> {
        self.permission_plan(&file_authority_context(ctx.clone())?, args)
    }

    async fn execute_with_file_authority(
        &self,
        ctx: ToolContext,
        call_id: String,
        args: Value,
    ) -> Result<ToolResult> {
        let call = ToolCall {
            id: call_id.clone(),
            name: self.spec().name,
            args_json: serde_json::to_string(&args)?,
        };
        let (ctx, _audit_root) = prepare_file_call(&file_registry(), ctx, &call)?;
        self.execute(ctx, call_id, args).await
    }

    async fn preview_with_file_authority(
        &self,
        ctx: ToolContext,
        args: Value,
    ) -> Result<Option<ToolPreview>> {
        self.preview(file_authority_context(ctx)?, args).await
    }
}

impl FileToolTestExt for ReadFileTool {}
impl FileToolTestExt for WriteFileTool {}
impl FileToolTestExt for EditFileTool {}
impl FileToolTestExt for DeleteFileTool {}
impl FileToolTestExt for ListTool {}
impl FileToolTestExt for GlobTool {}
impl FileToolTestExt for GrepTool {}

pub(crate) async fn execute_registered_file_call(
    registry: &ToolRegistry,
    ctx: ToolContext,
    call: ToolCall,
) -> Result<ToolResult> {
    let (ctx, _audit_root) = prepare_file_call(registry, ctx, &call)?;
    registry.execute(ctx, call).await
}
