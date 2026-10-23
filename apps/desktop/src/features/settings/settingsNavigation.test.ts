import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { useSettingsNavigation } from "./settingsNavigation";

describe("workspace-scoped settings navigation", () => {
  it("keeps separate navigation snapshots and binds late writes to their original workspace", () => {
    const { result, rerender } = renderHook(({ workspaceId }) => useSettingsNavigation(workspaceId), {
      initialProps: { workspaceId: "first" as string | undefined },
    });
    const rememberFirst = result.current.remember;
    act(() => rememberFirst({ category: "support", scrollTop: 120, focusKey: "open-support" }));
    rerender({ workspaceId: "second" });
    expect(result.current.value).toBeUndefined();
    act(() => result.current.remember({ category: "preferences", scrollTop: 40 }));
    act(() => rememberFirst({ category: "extensions", scrollTop: 80 }));
    rerender({ workspaceId: "first" });
    expect(result.current.value).toEqual({ category: "extensions", scrollTop: 80 });
    rerender({ workspaceId: "second" });
    expect(result.current.value).toEqual({ category: "preferences", scrollTop: 40 });
    rerender({ workspaceId: undefined });
    expect(result.current.value).toBeUndefined();
  });

  it("bounds the retained workspaces and normalizes negative scroll positions", () => {
    const { result, rerender } = renderHook(({ workspaceId }) => useSettingsNavigation(workspaceId), {
      initialProps: { workspaceId: "workspace-0" },
    });
    for (let index = 0; index < 17; index += 1) {
      rerender({ workspaceId: `workspace-${index}` });
      act(() => result.current.remember({ category: "connections", scrollTop: -1 }));
    }
    rerender({ workspaceId: "workspace-0" });
    expect(result.current.value).toBeUndefined();
    rerender({ workspaceId: "workspace-16" });
    expect(result.current.value).toEqual({ category: "connections", scrollTop: 0 });
  });
});
