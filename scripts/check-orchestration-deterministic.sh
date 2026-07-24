#!/usr/bin/env bash
set -euo pipefail

run_case() {
  local scenario="$1"
  local package="$2"
  local test_name="$3"

  printf 'orchestration acceptance: %s\n' "${scenario}"
  cargo test --locked -p "${package}" --lib "${test_name}"
}

run_case \
  "permission monotonicity" \
  "sigil-runtime" \
  "production_child_permission_materialization_preserves_ancestor_role_and_profile_caps"
run_case \
  "whole-batch zero-start" \
  "sigil-runtime" \
  "task_read_batch_rejects_member_preflight_before_any_provider_start"
run_case \
  "reverse completion" \
  "sigil-runtime" \
  "task_read_batch_overlaps_provider_runs_and_commits_in_request_order"
run_case \
  "429 bounded retry" \
  "sigil-kernel" \
  "read_step_rate_limit_stops_after_bounded_retry_budget"
run_case \
  "cancel and joined-child cleanup" \
  "sigil-runtime" \
  "root_cancellation_interrupts_all_joined_children_and_releases_slots"
run_case \
  "restart continuation" \
  "sigil-kernel" \
  "continue_consumes_one_durable_retry_schedule_after_restart"
run_case \
  "compaction recovery memory" \
  "sigil-kernel" \
  "compaction_extraction_keeps_active_task_objective_and_accepted_plan"
run_case \
  "model-owned guidance" \
  "sigil-kernel" \
  "guidance_review_replans_and_carries_completed_steps_forward"
run_case \
  "approval audit" \
  "sigil-runtime" \
  "child_tool_approval_routes_are_audited_and_stored"
run_case \
  "lane CAS" \
  "sigil-runtime" \
  "workspace_promotion_parent_drift_is_stale_with_zero_promotion_effect"
run_case \
  "partial promotion recovery" \
  "sigil-runtime" \
  "conflicting_lane_retains_partial_private_ref_without_parent_mutation"
run_case \
  "cleanup inventory" \
  "sigil-kernel" \
  "isolated_workspace_projection_reconstructs_cleanup_inventory_across_crash_windows"
