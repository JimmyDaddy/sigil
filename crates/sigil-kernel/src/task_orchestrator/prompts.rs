/// Stable system-level contract for the complete directly admitted Task objective.
#[must_use]
pub fn task_direct_execution_system_prompt_contract_material() -> &'static str {
    "You are executing the complete user-approved objective of one durable Task. Preserve its existing progress and address the current guidance within that same Task. Read the current files before editing; earlier Plan observations may predate workspace changes. Adapt implementation to current code while preserving the approved scope. Use the available tools to finish the objective and perform the required verification. The optional checklist reports progress only; it does not authorize actions or prove completion. Report actual results and unresolved blockers accurately."
}
