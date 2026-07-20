import { useEffect, useState } from "react";

import type { ThemePreference } from "./contract";
import { useAppearance } from "./ThemeProvider";
import { Button, Popover, Radio } from "../ui/primitives";

const choices: readonly { value: ThemePreference; label: string }[] = [
  { value: "system", label: "Follow system" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

export function AppearanceMenu() {
  const appearance = useAppearance();
  const [open, setOpen] = useState(false);

  useEffect(() => {
    const openAppearance = (event: KeyboardEvent) => {
      if (event.key !== "," || (!event.metaKey && !event.ctrlKey)) return;
      event.preventDefault();
      setOpen(true);
      window.requestAnimationFrame(() => {
        document.querySelector<HTMLInputElement>("input[name='desktop-theme']:checked")?.focus();
      });
    };
    window.addEventListener("keydown", openAppearance);
    return () => window.removeEventListener("keydown", openAppearance);
  }, []);

  return (
    <Popover
      className="appearance-menu"
      label="Appearance"
      open={open}
      onOpenChange={setOpen}
    >
      <div className="appearance-content">
        <fieldset disabled={appearance.status === "saving"}>
          <legend>Theme</legend>
          {choices.map((choice) => (
            <Radio
              key={choice.value}
              label={choice.label}
              name="desktop-theme"
              value={choice.value}
              checked={appearance.preference === choice.value}
              onChange={() => void appearance.setPreference(choice.value)}
            />
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
            <Button type="button" variant="danger" onClick={() => void appearance.retry()}>Retry</Button>
          </div>
        ) : null}
      </div>
    </Popover>
  );
}
