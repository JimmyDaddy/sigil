import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ConversationRecoveryPanel } from "./ConversationRecoveryPanel";
import { LocaleProvider } from "./i18n";

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

describe("ConversationRecoveryPanel compaction action", () => {
  it("offers one direct compaction action without preview or apply confirmation", async () => {
    const compact = vi.fn(async () => true);
    render(
      <LocaleProvider>
        <ConversationRecoveryPanel
          recovery={{ checkpoints: [], forkPoints: [], throughStreamSequence: 9 }}
          busy={false}
          error={false}
          onRefresh={vi.fn()}
          onCompact={compact}
          onPreview={vi.fn(async () => undefined)}
          onRestore={vi.fn(async () => undefined)}
          onFork={vi.fn(async () => undefined)}
        />
      </LocaleProvider>,
    );

    expect(screen.queryByRole("button", { name: /preview compaction/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /apply compaction/i })).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Compact now" }));
    expect(compact).toHaveBeenCalledOnce();
  });
});
