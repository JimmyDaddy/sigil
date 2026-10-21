import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ExtensionWorkbench } from "./ExtensionWorkbench";
import { LocaleProvider } from "./i18n";
import type { ExtensionCatalog } from "./types";

afterEach(() => { cleanup(); window.localStorage.clear(); });

const catalog: ExtensionCatalog = {
  commands: [],
  skills: [
    {
      id: "review",
      invocationToken: "$review",
      name: "Review",
      description: "Review code.",
      source: "workspace",
      runMode: "inline",
      trust: "trusted",
      available: true,
      binding: {
        skillId: "review",
        skillSha256: "skill-sha",
        indexFingerprint: "index-sha",
      },
    },
  ],
  agents: [
    {
      id: "explore",
      invocationToken: "@explore",
      name: "Explore",
      description: "Explore the workspace.",
      source: "system",
      kind: "subagent",
      trust: "trusted",
      enabled: true,
      userInvocable: true,
      available: false,
      unavailableReason: "A supervised owner is required.",
    },
  ],
};

describe("extension workbench", () => {
  it("binds an admitted skill into the composer", async () => {
    const user = userEvent.setup();
    const onUseSkill = vi.fn();
    render(
      <LocaleProvider>
        <ExtensionWorkbench
          catalog={catalog}
          runActive={false}
          onUseSkill={onUseSkill}
          onUseAgent={() => undefined}
        />
      </LocaleProvider>,
    );

    await user.click(screen.getByRole("button", { name: "Use in composer" }));
    expect(onUseSkill).toHaveBeenCalledWith(catalog.skills[0]);
  });

  it("shows agent policy honestly without enabling execution", async () => {
    const user = userEvent.setup();
    render(
      <LocaleProvider>
        <ExtensionWorkbench
          catalog={catalog}
          runActive={false}
          onUseSkill={() => undefined}
          onUseAgent={() => undefined}
        />
      </LocaleProvider>,
    );

    await user.click(screen.getByRole("tab", { name: "Agents 1" }));
    expect(screen.getByText("A supervised owner is required.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Select agent" }).hasAttribute("disabled")).toBe(true);
  });

  it("opens directly on a filtered agent requested by the composer", () => {
    render(
      <LocaleProvider>
        <ExtensionWorkbench
          catalog={catalog}
          runActive={false}
          initialKind="agents"
          initialQuery="expl"
          onUseSkill={() => undefined}
          onUseAgent={() => undefined}
        />
      </LocaleProvider>,
    );

    expect(screen.getByRole("tab", { name: "Agents 1" }).getAttribute("aria-selected")).toBe("true");
    expect((screen.getByRole("textbox") as HTMLInputElement).value).toBe("expl");
    expect(screen.getByText("@explore")).toBeTruthy();
  });

  it("binds an admitted agent into the composer", async () => {
    const user = userEvent.setup();
    const availableAgent = {
      ...catalog.agents[0],
      available: true,
      unavailableReason: undefined,
      binding: { profileId: "explore", snapshotId: "agent-snapshot" },
    };
    const onUseAgent = vi.fn();
    render(
      <LocaleProvider>
        <ExtensionWorkbench
          catalog={{ ...catalog, agents: [availableAgent] }}
          runActive={false}
          initialKind="agents"
          onUseSkill={() => undefined}
          onUseAgent={onUseAgent}
        />
      </LocaleProvider>,
    );

    await user.click(screen.getByRole("button", { name: "Select agent" }));
    expect(onUseAgent).toHaveBeenCalledWith(availableAgent);
  });
});

describe("extension presentation", () => {
  it("distinguishes an empty catalog from a search with no matches and removes the nonexistent selection pane", async () => {
    const user = userEvent.setup();
    const props = { runActive: false, onUseSkill: vi.fn(), onUseAgent: vi.fn() };
    const { rerender } = render(<ExtensionWorkbench {...props} catalog={{ commands: [], skills: [], agents: [] }} />);
    expect(screen.getByRole("status").textContent).toContain("No skills or agents are available");
    expect(screen.queryByText(/Select an item/)).toBeNull();
    expect(screen.queryByText("No matching extensions.")).toBeNull();
    rerender(<ExtensionWorkbench {...props} catalog={catalog} />);
    await user.type(screen.getByRole("textbox"), "does-not-exist");
    expect(screen.getByRole("status").textContent).toContain("No matching extensions.");
    expect(screen.queryByRole("button", { name: "Use in composer" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Clear search" }));
    expect(await screen.findByRole("button", { name: "Use in composer" })).toBeTruthy();
    expect((screen.getByRole("textbox") as HTMLInputElement).value).toBe("");
  });

  it("distinguishes an empty kind while keeping the other catalog accessible", async () => {
    const user = userEvent.setup();
    render(<ExtensionWorkbench catalog={{ ...catalog, skills: [] }} runActive={false} onUseSkill={vi.fn()} onUseAgent={vi.fn()} />);
    expect(screen.getByRole("status").textContent).toContain("No skills are available");
    await user.click(screen.getByRole("tab", { name: "Agents 1" }));
    expect(screen.getByText("A supervised owner is required.")).toBeTruthy();
    expect((screen.getByRole("button", { name: "Select agent" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("localizes known values, retains original details, and does not reinterpret future values", async () => {
    window.localStorage.setItem("sigil.desktop.locale.v1", "zh-CN");
    const available = { ...catalog.agents[0], available: true, kind: "primary", binding: { profileId: "explore", snapshotId: "snapshot" } };
    const props = { runActive: false, initialKind: "agents" as const, onUseSkill: vi.fn(), onUseAgent: vi.fn() };
    const { rerender } = render(<LocaleProvider><ExtensionWorkbench {...props} catalog={{ ...catalog, agents: [available] }} /></LocaleProvider>);
    const article = screen.getByRole("article");
    const facts = within(article.querySelector(":scope > dl")! as HTMLElement);
    expect(facts.getByText("内置")).toBeTruthy();
    expect(facts.getByText("已信任")).toBeTruthy();
    expect(facts.getByText("主 Agent")).toBeTruthy();
    expect(screen.getByRole("button", { name: "选择 Agent" })).toBeTruthy();
    expect(article.querySelector("details")?.textContent).toContain("system");
    rerender(<LocaleProvider><ExtensionWorkbench {...props} catalog={{ ...catalog, agents: [{ ...available, source: "future/source", trust: "future-trust", kind: "future-kind" }] }} /></LocaleProvider>);
    const futureFacts = within(screen.getByRole("article").querySelector(":scope > dl")! as HTMLElement);
    for (const value of ["future/source", "future-trust", "future-kind"]) expect(futureFacts.getByText(value)).toBeTruthy();
  });
});
