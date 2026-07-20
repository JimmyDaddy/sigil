import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ExtensionWorkbench } from "./ExtensionWorkbench";
import { LocaleProvider } from "./i18n";
import type { ExtensionCatalog } from "./types";

afterEach(cleanup);

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
        <ExtensionWorkbench catalog={catalog} runActive={false} onUseSkill={onUseSkill} />
      </LocaleProvider>,
    );

    await user.click(screen.getByRole("button", { name: "Use in composer" }));
    expect(onUseSkill).toHaveBeenCalledWith(catalog.skills[0]);
  });

  it("shows agent policy honestly without enabling execution", async () => {
    const user = userEvent.setup();
    render(
      <LocaleProvider>
        <ExtensionWorkbench catalog={catalog} runActive={false} onUseSkill={() => undefined} />
      </LocaleProvider>,
    );

    await user.click(screen.getByRole("tab", { name: "Agents 1" }));
    expect(screen.getByText("A supervised owner is required.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Start agent" }).hasAttribute("disabled")).toBe(true);
  });
});
