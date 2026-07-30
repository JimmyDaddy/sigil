import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { DesktopBridge } from "../../bridge";
import type { DesktopUpdateSnapshot } from "../../types";
import { LocaleProvider } from "../../i18n";
import { DesktopUpdateCard } from "./DesktopUpdateCard";

afterEach(cleanup);

const available: DesktopUpdateSnapshot = {
  phase: "available",
  channel: "beta",
  currentVersion: "0.0.1-beta.1",
  version: "0.0.1-beta.2",
  notes: "Signed Desktop beta",
  downloadedBytes: 0,
};

function updateBridge(overrides: Partial<DesktopBridge> = {}): DesktopBridge {
  return {
    updateState: async () => available,
    checkForUpdate: async () => available,
    downloadAndInstallUpdate: async () => ({
      ...available,
      phase: "ready_to_restart",
      downloadedBytes: 42,
      totalBytes: 42,
    }),
    restartAfterUpdate: async () => undefined,
    subscribeUpdate: async () => () => undefined,
    ...overrides,
  } as DesktopBridge;
}

describe("DesktopUpdateCard", () => {
  it("requires an explicit download/install action and keeps restart explicit", async () => {
    const downloadAndInstallUpdate = vi.fn(updateBridge().downloadAndInstallUpdate);
    const restartAfterUpdate = vi.fn(async () => undefined);
    render(
      <LocaleProvider>
        <DesktopUpdateCard bridge={updateBridge({
          downloadAndInstallUpdate,
          restartAfterUpdate,
        })} />
      </LocaleProvider>,
    );

    const user = userEvent.setup();
    const install = await screen.findByRole("button", { name: "Download and install" });
    expect(downloadAndInstallUpdate).not.toHaveBeenCalled();
    await user.click(install);

    expect(downloadAndInstallUpdate).toHaveBeenCalledOnce();
    expect(await screen.findByRole("button", { name: "Restart Sigil" })).toBeTruthy();
    expect(screen.getByText(/keep working and restart when you are ready/i)).toBeTruthy();
    expect(restartAfterUpdate).not.toHaveBeenCalled();
  });

  it("keeps the installed update ready when an active task blocks restart", async () => {
    const blocked: DesktopUpdateSnapshot = {
      ...available,
      phase: "ready_to_restart",
      errorCode: "update_restart_blocked",
    };
    const updateState = vi.fn()
      .mockResolvedValueOnce({ ...available, phase: "ready_to_restart" })
      .mockResolvedValue(blocked);
    render(
      <LocaleProvider>
        <DesktopUpdateCard bridge={updateBridge({
          updateState,
          restartAfterUpdate: async () => {
            throw { code: "update_restart_blocked" };
          },
        })} />
      </LocaleProvider>,
    );

    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Restart Sigil" }));

    await waitFor(() => {
      expect(screen.getByText(/Finish or cancel active tasks/i)).toBeTruthy();
    });
    expect(screen.getByRole("button", { name: "Restart Sigil" })).toBeTruthy();
  });

  it("keeps download progress indeterminate until the server reports a total size", async () => {
    render(
      <LocaleProvider>
        <DesktopUpdateCard bridge={updateBridge({
          updateState: async () => ({
            ...available,
            phase: "downloading",
            downloadedBytes: 4_096,
          }),
        })} />
      </LocaleProvider>,
    );

    const progress = await screen.findByRole("progressbar", {
      name: "Desktop update download progress",
    });
    expect(progress.hasAttribute("value")).toBe(false);
    expect(progress.hasAttribute("max")).toBe(false);
    expect(screen.getByText("4.0 KiB")).toBeTruthy();
  });

  it("keeps an update found by the background event available in Settings", async () => {
    let publish: ((snapshot: DesktopUpdateSnapshot) => void) | undefined;
    render(
      <LocaleProvider>
        <DesktopUpdateCard bridge={updateBridge({
          updateState: async () => ({
            phase: "idle",
            channel: "beta",
            currentVersion: available.currentVersion,
            downloadedBytes: 0,
          }),
          subscribeUpdate: async (listener) => {
            publish = listener;
            return () => undefined;
          },
        })} />
      </LocaleProvider>,
    );

    expect(await screen.findByText("Beta channel")).toBeTruthy();
    await act(async () => publish?.(available));

    expect(screen.getByText("Update available")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Download and install" })).toBeTruthy();
  });
});
