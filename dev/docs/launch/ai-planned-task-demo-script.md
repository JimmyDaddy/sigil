# Sigil AI-Planned Task Demo Script

This is the canonical recording plan for showing Sigil turning one repository
goal into an accepted multi-step Task. It complements the 45-second focused-edit
demo; it does not replace or retroactively relabel that footage.

The recording must use a real Sigil TUI run in a disposable repository.
Provider waits may be shortened during editing, but the plan, Task state,
parallel steps, tool approvals, and verification result must not be mocked.

## Message

**One-line promise:** Give Sigil the goal. Review the plan. Follow parallel work
to a verified result.

The demo proves five things in order:

1. Sigil generates a bounded execution plan from a user goal.
2. The user can inspect and accept the plan before execution.
3. Independent steps can run at the same time while their state stays visible.
4. Plan acceptance does not bypass file, command, network, or external-tool
   approval.
5. The Task closes only after the repository-owned verification check passes.

## Recording Setup

- Record at 1920 × 1080, 30 or 60 fps, with a fixed terminal viewport.
- Use a disposable repository containing English and Chinese release notes, a
  small static homepage, a deterministic screenshot-generation command, and a
  fast repository-owned documentation check.
- Start in manual permission mode. Hide provider balance, API keys, usernames,
  home-directory paths, and unrelated sessions.
- Use `/plan` for the public recording so plan review is unambiguous. A separate
  optional cut may show an explicitly invoked `/task`.
- Capture the complete real run before editing. Dead provider time may be cut,
  but step order, concurrent activity, approvals, and results must remain
  faithful to the recorded session.
- Do not claim ordinary chat automatically enters the Task flow on every
  installation. That behavior is limited to qualified new-install routes and is
  visible through `sigil doctor`.

## Exact Demo Prompt

```text
Prepare this repository for the alpha.6 documentation release.
Update the English and Chinese release notes together, refresh the homepage
capability summary and generated TUI screenshots, then run the repository-owned
documentation check. Keep product code unchanged and show every proposed write.
```

The generated plan should contain a short scope-confirmation step, two
independent update steps that can run in parallel, and one final verification
step. If the generated plan does not preserve those boundaries, reject it and
record a new real run rather than editing the plan text into the footage.

## Suggested Timeline

| Time | Picture and action | On-screen caption / voiceover |
| --- | --- | --- |
| 00:00–00:05 | Enter the exact `/plan` prompt. | `One repository goal.` |
| 00:05–00:15 | Hold on the Plan ready card; inspect target paths, steps, and suggested check. | `Sigil turns the goal into a bounded plan.` |
| 00:15–00:20 | Accept the plan and create the Task. | `You decide when execution starts.` |
| 00:20–00:38 | Show the Task strip with the docs and screenshot steps active together. | `Independent work runs in parallel and stays visible.` |
| 00:38–00:48 | Pause on one real write approval and approve once. | `The plan does not replace tool approval.` |
| 00:48–01:00 | Show both steps complete, the final check pass, and the verified Task result. | `The Task finishes with evidence.` |

## Acceptance Checklist

- Every visible interaction comes from the real TUI.
- The Plan ready card is readable and shows the actual proposed steps.
- At least two independent Task steps are visibly active during the same run.
- A real risky action stops at the normal approval surface.
- The final verification command and passing result are visible.
- The recording contains no secret, personal path, provider balance, or
  unrelated repository data.
- Captions say “qualified new-install routes” if automatic ordinary-chat routing
  is mentioned.
- Published captions cover the complete edited run.
