import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { LoadingState } from ".";

afterEach(cleanup);

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
});
