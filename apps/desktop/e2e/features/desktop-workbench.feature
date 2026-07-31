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
    Then the initial run and queued follow-up both complete
    And the generated semantic title is synchronized into the conversation page

  Scenario: Load and execute workspace extensions
    Given the current-source desktop has restored the isolated workspace
    When I create a new desktop conversation
    And I invoke the custom workspace skill
    Then the custom workspace skill executes with durable load evidence
    When I create a new desktop conversation
    And I invoke the custom workspace agent
    Then the custom workspace agent executes with its profile instructions

  Scenario: Execute the supervised plan agent
    Given the current-source desktop has restored the isolated workspace
    When I create a new desktop conversation
    And I invoke Desktop plan mode
    Then the supervised plan agent executes with durable profile evidence

  Scenario: Automatically plan and dispatch parallel Agents
    Given the current-source desktop has restored the isolated workspace
    When I create a new desktop conversation
    And I request automatic multi-Agent execution
    Then Desktop completes one durable task with two overlapping read Agents

  Scenario: Delete a conversation source that the current runtime cannot open
    Given the current-source desktop has restored the isolated workspace
    When I create a new desktop conversation
    And an unsupported conversation source is stored in the workspace
    Then I can permanently delete the unavailable source from conversation management
