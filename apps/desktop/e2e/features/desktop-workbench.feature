@desktop @native @smoke
Feature: Desktop workbench remains usable
  The source-built desktop application must exercise the real Tauri bridge
  against an isolated local workspace before a release is published.

  Scenario: Restore a workspace and create a conversation
    Given the current-source desktop has restored the isolated workspace
    When I create a new desktop conversation
    Then the conversation timeline and composer are usable
    And the timeline content and composer are horizontally aligned
    When I start a run that requires approval
    Then the approval remains live without losing runtime control
    When I press Enter with a follow-up while approval is pending
    Then the follow-up is recorded in the durable queue
    When I approve the pending command
    Then the accepted approval stops offering actions before the next run event
    Then the initial run and queued follow-up both complete
    And terminal completion releases continuity and history controls
    And the generated semantic title is synchronized into the conversation page

  Scenario: Load and execute workspace extensions
    Given the current-source desktop has restored the isolated workspace
    When I create a new desktop conversation
    And I invoke the custom workspace skill
    Then the custom workspace skill executes with durable load evidence
    When I create a new desktop conversation
    And I invoke the custom workspace agent
    Then the custom workspace agent executes with its profile instructions

  Scenario: Keep a persistent terminal task live after foreground completion
    Given the current-source desktop has restored the isolated workspace
    When I create a new desktop conversation
    And I start a persistent terminal task from Desktop
    Then the terminal task becomes ready before the foreground answer completes
    And the foreground answer completes while the terminal task remains running
    When I start a successor run while the older terminal task remains running
    Then the successor completes while the older terminal task is still tracked
    And the later terminal exit settles the Desktop task card

  Scenario: Stop a persistent terminal task after foreground completion
    Given the current-source desktop has restored the isolated workspace
    When I create a new desktop conversation
    And I start a persistent terminal task from Desktop
    Then the terminal task becomes ready before the foreground answer completes
    And the foreground answer completes while the terminal task remains running
    When I stop the retained terminal task
    Then the retained terminal task becomes cancelled

  Scenario: Restore a retained terminal task after a renderer reload
    Given the current-source desktop has restored the isolated workspace
    When I create a new desktop conversation
    And I start a persistent terminal task from Desktop
    Then the terminal task becomes ready before the foreground answer completes
    And the foreground answer completes while the terminal task remains running
    When I reload Desktop while the retained terminal task is owned by the session
    Then Desktop restores the retained terminal task from continuity

  Scenario: Execute the supervised plan agent
    Given the current-source desktop has restored the isolated workspace
    When I create a new desktop conversation
    And I invoke Desktop plan mode
    Then the supervised plan agent drafts a durable plan
    And the draft plan becomes ready on the Desktop plan card
    When I save the reviewed plan from Desktop
    Then the plan card closes without creating a Task

  Scenario: Automatically review and run a draft plan with parallel Agents
    Given the current-source desktop has restored the isolated workspace
    When I create a new desktop conversation
    And I request automatic multi-Agent execution
    Then the automatic plan review drafts a durable plan
    And the draft plan becomes ready on the Desktop plan card
    When I run the reviewed plan from Desktop
    Then Desktop completes one durable task with two overlapping read Agents

  Scenario: Delete a conversation source that the current runtime cannot open
    Given the current-source desktop has restored the isolated workspace
    When I create a new desktop conversation
    And an unsupported conversation source is stored in the workspace
    Then I can permanently delete the unavailable source from conversation management

  Scenario: Recover an invalid provider configuration without losing the workspace
    Given the current-source desktop has restored the isolated workspace
    When the provider configuration becomes invalid and Desktop restarts
    Then the workspace opens in provider configuration recovery
    When I explicitly replace the invalid provider configuration
    Then the repaired workspace can create a new conversation
