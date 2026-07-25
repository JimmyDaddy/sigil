import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { LocaleProvider } from "../../i18n";
import { MermaidDiagram } from "../MermaidDiagram";
import { clearDiagramCache } from "../renderCache";

const mermaidMock = vi.hoisted(() => ({
  initialize: vi.fn(),
  render: vi.fn(async (id: string) => ({
    svg: [
      `<svg xmlns="http://www.w3.org/2000/svg" id="${id}">`,
      `<style>#${id}{color:#222}#${id} .node{fill:#fff}</style>`,
      '<g class="node"><text>Rendered diagram</text></g>',
      "</svg>",
    ].join(""),
  })),
}));

vi.mock("mermaid", () => ({ default: mermaidMock }));

afterEach(() => {
  cleanup();
  clearDiagramCache();
  mermaidMock.initialize.mockClear();
  mermaidMock.render.mockClear();
});

function renderDiagram(source: string) {
  return render(
    <LocaleProvider>
      <MermaidDiagram source={source} contentId="message-1" />
    </LocaleProvider>,
  );
}

describe("Mermaid diagram card", () => {
  it("renders locally with strict settings and exposes bounded source actions", async () => {
    const user = userEvent.setup();
    const { container } = renderDiagram("flowchart TD\nA-->B");
    await screen.findByText("flowchart · ready");
    expect(mermaidMock.initialize).toHaveBeenCalledWith(expect.objectContaining({
      securityLevel: "strict",
      startOnLoad: false,
      suppressErrorRendering: true,
    }));
    expect(container.querySelector("svg")).not.toBeNull();
    expect(screen.getByRole("img", { name: "Diagram: flowchart" })).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Show source" }));
    expect(screen.getByText("flowchart TD", { exact: false })).toBeTruthy();
  });

  it("falls back locally for a rejected directive without loading Mermaid", async () => {
    renderDiagram("%%{init: {}}%%\nflowchart TD\nA-->B");
    expect(await screen.findByText(/exceeds the safe local rendering policy/i)).toBeTruthy();
    await waitFor(() => expect(mermaidMock.render).not.toHaveBeenCalled());
    expect(screen.getByText("%%{init:", { exact: false })).toBeTruthy();
  });

  it("does not commit a stale render after the source changes", async () => {
    let resolveFirst: ((value: { svg: string }) => void) | undefined;
    mermaidMock.render
      .mockImplementationOnce((id: string) => new Promise((resolve) => {
        resolveFirst = resolve;
      }))
      .mockImplementationOnce(async (id: string) => ({
        svg: `<svg xmlns="http://www.w3.org/2000/svg" id="${id}"><text>Second diagram</text></svg>`,
      }));

    const view = renderDiagram("flowchart TD\nA-->B");
    await waitFor(() => expect(mermaidMock.render).toHaveBeenCalledTimes(1));
    view.rerender(
      <LocaleProvider>
        <MermaidDiagram source="flowchart TD\nB-->C" contentId="message-1" />
      </LocaleProvider>,
    );
    resolveFirst?.({
      svg: `<svg xmlns="http://www.w3.org/2000/svg" id="${
        mermaidMock.render.mock.calls[0]?.[0] as string
      }"><text>First diagram</text></svg>`,
    });

    await waitFor(() => expect(mermaidMock.render).toHaveBeenCalledTimes(2));
    await screen.findByText("Second diagram");
    expect(screen.queryByText("First diagram")).toBeNull();
  });

  it("rerenders with a separate cache identity after a theme change", async () => {
    document.documentElement.dataset.theme = "sigil_dark";
    const view = renderDiagram("flowchart TD\nA-->B");
    await screen.findByText("Rendered diagram");
    expect(mermaidMock.render).toHaveBeenCalledTimes(1);

    document.documentElement.dataset.theme = "solarized_light";
    view.rerender(
      <LocaleProvider>
        <MermaidDiagram source="flowchart TD\nA-->B" contentId="message-1" />
      </LocaleProvider>,
    );
    await waitFor(() => expect(mermaidMock.render).toHaveBeenCalledTimes(2));
    delete document.documentElement.dataset.theme;
  });
});
