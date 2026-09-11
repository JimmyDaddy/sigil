import { act, fireEvent, render, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { LocaleProvider } from "./i18n";
import { MessageContentPager, type ReadMessageContent } from "./MessageContentPager";
import type { MessageContentPage } from "./types";

function page(offset: number, text: string, nextOffset: number | null): MessageContentPage {
  return { displayId: "message-1", messageId: "durable-1", contentVersion: "version-1",
    offset, text, nextOffset, totalBytes: 65_540 };
}

function mount(onRead: ReadMessageContent) {
  return render(<LocaleProvider><MessageContentPager displayId="message-1" onRead={onRead} /></LocaleProvider>);
}

describe("saved message content paging", () => {
  it("replaces each page and preserves the exact version and offsets when navigating", async () => {
    const onRead = vi.fn<ReadMessageContent>(async (request) => request.offset === 65_536
      ? page(65_536, "尾x", null) : page(0, "x".repeat(65_536), 65_536));
    const view = mount(onRead);
    fireEvent.click(view.container.querySelector("[data-message-content-toggle]")!);
    await waitFor(() => expect(view.container.querySelector("[data-message-content-page]")?.textContent?.length).toBe(65_536));
    fireEvent.click(view.container.querySelector("[data-message-content-next]")!);
    await waitFor(() => expect(view.container.querySelector("[data-message-content-page]")?.textContent).toBe("尾x"));
    expect(onRead.mock.calls[1]?.[0]).toEqual({ displayId: "message-1", offset: 65_536,
      limit: 65_536, contentVersion: "version-1" });
    expect((view.container.querySelector("[data-message-content-next]") as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(view.container.querySelector("[data-message-content-previous]")!);
    await waitFor(() => expect(view.container.querySelector("[data-message-content-page]")?.textContent?.length).toBe(65_536));
    expect(onRead.mock.calls[2]?.[0].contentVersion).toBe("version-1");
    expect(view.container.querySelectorAll("[data-message-content-page]")).toHaveLength(1);
  });

  it("aborts an in-flight native query when closed and ignores its late response", async () => {
    let resolvePage: ((value: MessageContentPage) => void) | undefined;
    const onRead = vi.fn<ReadMessageContent>(() => new Promise((resolve) => { resolvePage = resolve; }));
    const view = mount(onRead);
    fireEvent.click(view.container.querySelector("[data-message-content-toggle]")!);
    await waitFor(() => expect(onRead).toHaveBeenCalledTimes(1));
    const signal = onRead.mock.calls[0]![1];
    fireEvent.click(view.container.querySelector("[data-message-content-toggle]")!);
    expect(signal.aborted).toBe(true);
    await act(async () => resolvePage?.(page(0, "x".repeat(65_536), 65_536)));
    expect(view.container.querySelector("[data-message-content-page]")).toBeNull();
  });

  it("rejects a changed content version instead of mixing pages", async () => {
    const onRead = vi.fn<ReadMessageContent>(async (request) => request.offset === 65_536
      ? { ...page(65_536, "尾x", null), contentVersion: "different-version" }
      : page(0, "x".repeat(65_536), 65_536));
    const view = mount(onRead);
    fireEvent.click(view.container.querySelector("[data-message-content-toggle]")!);
    await waitFor(() => expect(view.container.querySelector("[data-message-content-page]")).not.toBeNull());
    fireEvent.click(view.container.querySelector("[data-message-content-next]")!);
    await waitFor(() => expect(view.container.querySelector('[role="alert"]')).not.toBeNull());
    expect(view.container.querySelector("[data-message-content-page]")).toBeNull();
  });
});
