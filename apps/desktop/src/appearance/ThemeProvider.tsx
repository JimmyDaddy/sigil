import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import type { DesktopBridge } from "../bridge";
import type { AppearanceSnapshot, ThemePreference } from "./contract";
import { appearanceFromDocument, applyAppearance } from "./resolveTheme";

type AppearanceStatus = "idle" | "saving" | "error";

interface AppearanceContextValue extends AppearanceSnapshot {
  status: AppearanceStatus;
  error?: string;
  setPreference(preference: ThemePreference): Promise<void>;
  retry(): Promise<void>;
  sync(snapshot: AppearanceSnapshot): void;
}

const AppearanceContext = createContext<AppearanceContextValue | undefined>(undefined);

export interface ThemeProviderProps {
  readonly bridge: DesktopBridge;
  readonly children: ReactNode;
}

export function ThemeProvider({ bridge, children }: ThemeProviderProps) {
  const [appearance, setAppearance] = useState(appearanceFromDocument);
  const appearanceRef = useRef(appearance);
  const [status, setStatus] = useState<AppearanceStatus>("idle");
  const [error, setError] = useState<string>();
  const retryPreference = useRef<ThemePreference>(appearance.preference);

  const sync = useCallback((snapshot: AppearanceSnapshot) => {
    const changed =
      appearanceRef.current.preference !== snapshot.preference ||
      appearanceRef.current.resolvedTheme !== snapshot.resolvedTheme;
    applyAppearance(snapshot);
    appearanceRef.current = snapshot;
    setAppearance(snapshot);
    if (changed) {
      setStatus("idle");
      setError(undefined);
    }
    retryPreference.current = snapshot.preference;
  }, []);

  const setPreference = useCallback(async (preference: ThemePreference) => {
    retryPreference.current = preference;
    setStatus("saving");
    setError(undefined);
    try {
      sync(await bridge.setAppearance(preference));
    } catch {
      setStatus("error");
      setError("Theme could not be saved. The previous appearance is still active.");
    }
  }, [bridge, sync]);

  const retry = useCallback(
    () => setPreference(retryPreference.current),
    [setPreference],
  );

  useEffect(() => {
    let disposed = false;
    let unsubscribe: (() => void) | undefined;
    void bridge.subscribeAppearance((snapshot) => {
      if (!disposed) sync(snapshot);
    }).then((next) => {
      if (disposed) next();
      else unsubscribe = next;
    });
    return () => {
      disposed = true;
      unsubscribe?.();
    };
  }, [bridge, sync]);

  const value = useMemo<AppearanceContextValue>(() => ({
    ...appearance,
    status,
    error,
    setPreference,
    retry,
    sync,
  }), [appearance, error, retry, setPreference, status, sync]);

  return <AppearanceContext.Provider value={value}>{children}</AppearanceContext.Provider>;
}

export function useAppearance(): AppearanceContextValue {
  const context = useContext(AppearanceContext);
  if (context === undefined) throw new Error("useAppearance must be used within ThemeProvider");
  return context;
}
