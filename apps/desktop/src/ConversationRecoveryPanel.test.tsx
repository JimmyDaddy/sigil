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

  it("maps loading and failed recovery states and keeps retry explicit", async () => {
    const user = userEvent.setup();
    const onRefresh = vi.fn();
    const props = {
      busy: false,
      error: false,
      onRefresh,
      onCompact: vi.fn(async () => true),
      onPreview: vi.fn(async () => undefined),
      onRestore: vi.fn(async () => undefined),
      onFork: vi.fn(async () => undefined),
    };

    const { rerender } = render(
      <LocaleProvider>
        <ConversationRecoveryPanel {...props} />
      </LocaleProvider>,
    );
    expect(screen.getByTestId("recovery-state").getAttribute("data-recovery-product-status")).toBe("loading");
    expect(screen.getByText("Loading")).toBeTruthy();

    rerender(
      <LocaleProvider>
        <ConversationRecoveryPanel {...props} error />
      </LocaleProvider>,
    );
    expect(screen.getByTestId("recovery-state").getAttribute("data-recovery-product-status")).toBe("failed");
    expect(screen.getByText("Failed")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Retry" }));
    expect(onRefresh).toHaveBeenCalledOnce();
  });
});
