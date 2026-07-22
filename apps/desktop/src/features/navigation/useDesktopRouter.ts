import { useCallback, useState } from "react";

export type DesktopRoute = "conversation" | "library" | "settings" | "support";

export const DESKTOP_ROUTE_MAP: Readonly<Record<DesktopRoute, {
  readonly parent?: DesktopRoute;
  readonly primary: "conversation" | "library" | "settings";
  readonly requiresWorkspace: boolean;
}>> = {
  conversation: { primary: "conversation", requiresWorkspace: false },
  library: { primary: "library", requiresWorkspace: true },
  settings: { primary: "settings", requiresWorkspace: false },
  support: { parent: "settings", primary: "settings", requiresWorkspace: true },
};

export function useDesktopRouter() {
  const [route, setRoute] = useState<DesktopRoute>("conversation");

  const navigate = useCallback((next: DesktopRoute) => setRoute(next), []);
  const back = useCallback(() => {
    setRoute((current) => DESKTOP_ROUTE_MAP[current].parent ?? "conversation");
  }, []);

  return { route, navigate, back } as const;
}
