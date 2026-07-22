import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { LoadingState, NotificationProvider, PaginationControl, useNotifications } from ".";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("Sigil feedback patterns", () => {
  it("exposes a branded, accessible loading state", () => {
    const { container } = render(
      <LoadingState label="Opening conversation…" detail="Restoring saved messages." />,
    );

    const loading = screen.getByRole("status", { name: "Opening conversation…" });
    expect(loading.getAttribute("aria-busy")).toBe("true");
    expect(loading.textContent).toContain("Restoring saved messages.");
    expect(container.querySelectorAll(".sg-brand-loader-mark img")).toHaveLength(2);
  });

  it("centers pagination and swaps to the compact branded loading signal", () => {
    const onLoadMore = vi.fn();
    const { rerender } = render(
      <PaginationControl label="Load more" loadingLabel="Loading" loading={false} onLoadMore={onLoadMore} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Load more" }));
    expect(onLoadMore).toHaveBeenCalledOnce();
    expect(document.querySelector(".sg-pagination-control")).toBeTruthy();

    rerender(<PaginationControl label="Load more" loadingLabel="Loading" loading onLoadMore={onLoadMore} />);
    const loading = screen.getByRole("button", { name: "Loading" });
    expect(loading.getAttribute("aria-busy")).toBe("true");
    expect(loading.querySelectorAll(".sg-pagination-loader-mark")).toHaveLength(2);
  });
});

describe("Sigil notification center", () => {
  it("deduplicates repeated feedback and expires it after the tone timeout", () => {
    vi.useFakeTimers();
    render(
      <NotificationProvider>
        <NotificationFixture />
      </NotificationProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "Notify success" }));
    fireEvent.click(screen.getByRole("button", { name: "Notify success" }));
    expect(screen.getAllByText("Saved locally")).toHaveLength(1);

    act(() => vi.advanceTimersByTime(3_999));
    expect(screen.getByText("Saved locally")).toBeTruthy();
    act(() => vi.advanceTimersByTime(1));
    expect(screen.queryByText("Saved locally")).toBeNull();
  });

  it("keeps only the four most recent notifications", () => {
    render(
      <NotificationProvider>
        <NotificationFixture />
      </NotificationProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "Fill notifications" }));
    expect(screen.queryByText("Notice 1")).toBeNull();
    expect(screen.queryByText("Notice 2")).toBeNull();
    expect(screen.getByText("Notice 3")).toBeTruthy();
    expect(screen.getByText("Notice 6")).toBeTruthy();
    expect(screen.getAllByRole("status")).toHaveLength(4);
  });
});

function NotificationFixture() {
  const { notify } = useNotifications();
  return (
    <>
      <button type="button" onClick={() => notify({ message: "Saved locally", tone: "success" })}>
        Notify success
      </button>
      <button
        type="button"
        onClick={() => {
          for (let index = 1; index <= 6; index += 1) {
            notify({ message: `Notice ${index}`, timeoutMs: 0 });
          }
        }}
      >
        Fill notifications
      </button>
    </>
  );
}
