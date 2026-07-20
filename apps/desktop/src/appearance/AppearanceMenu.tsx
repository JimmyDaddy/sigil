import { useEffect, useRef } from "react";

import type { ThemePreference } from "./contract";
import { useAppearance } from "./ThemeProvider";

const choices: readonly { value: ThemePreference; label: string }[] = [
  { value: "system", label: "Follow system" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

export function AppearanceMenu() {
  const appearance = useAppearance();
  const detailsRef = useRef<HTMLDetailsElement>(null);

  useEffect(() => {
    const openAppearance = (event: KeyboardEvent) => {
      if (event.key !== "," || (!event.metaKey && !event.ctrlKey)) return;
      event.preventDefault();
      const details = detailsRef.current;
      if (details === null) return;
      details.open = true;
      window.requestAnimationFrame(() => {
        details.querySelector<HTMLInputElement>("input:checked")?.focus();
      });
    };
    window.addEventListener("keydown", openAppearance);
    return () => window.removeEventListener("keydown", openAppearance);
  }, []);

  return (
    <details className="appearance-menu" ref={detailsRef}>
      <summary>Appearance</summary>
      <div className="appearance-popover">
        <fieldset disabled={appearance.status === "saving"}>
          <legend>Theme</legend>
          {choices.map((choice) => (
            <label key={choice.value}>
              <input
                type="radio"
                name="desktop-theme"
                value={choice.value}
                checked={appearance.preference === choice.value}
                onChange={() => void appearance.setPreference(choice.value)}
              />
              <span>{choice.label}</span>
            </label>
          ))}
        </fieldset>
        <small>
          {appearance.preference === "system"
            ? `System currently resolves to ${appearance.resolvedTheme}.`
            : `${appearance.resolvedTheme[0].toUpperCase()}${appearance.resolvedTheme.slice(1)} is active.`}
        </small>
        {appearance.error !== undefined ? (
          <div className="appearance-error" role="alert">
            <span>{appearance.error}</span>
            <button type="button" onClick={() => void appearance.retry()}>Retry</button>
          </div>
        ) : null}
      </div>
    </details>
  );
}
