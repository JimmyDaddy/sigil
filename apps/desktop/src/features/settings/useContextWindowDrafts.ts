import { useCallback, useState } from "react";

export interface ContextWindowDrafts {
  readonly values: Readonly<Record<string, string>>;
  readonly update: (scope: string, value: string, baseline: string) => void;
  readonly discard: (scope: string, expectedValue?: string) => void;
}

/** App-owned, memory-only edits; no credentials or automatic configuration writes. */
export function useContextWindowDrafts(): ContextWindowDrafts {
  const [values, setValues] = useState<Record<string, string>>({});
  const discard = useCallback((scope: string, expectedValue?: string) => {
    setValues((current) => {
      if (!(scope in current)) return current;
      if (expectedValue !== undefined && current[scope] !== expectedValue) return current;
      const next = { ...current };
      delete next[scope];
      return next;
    });
  }, []);
  const update = useCallback((scope: string, value: string, baseline: string) => {
    setValues((current) => {
      const next = { ...current };
      if (value.trim() === baseline) delete next[scope];
      else next[scope] = value;
      return next;
    });
  }, []);
  return { values, update, discard };
}
