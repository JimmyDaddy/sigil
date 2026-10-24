import { createRef } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";

import { LocaleProvider } from "../../i18n";
import type { ProviderConnection } from "../../types";
import { CustomModelDialog } from "./CustomModelDialog";

afterEach(() => { cleanup(); window.localStorage.clear(); });

const connections: ProviderConnection[] = [{
  id: "custom-env", label: "Custom gateway", providerLabel: "Custom", protocolLabel: "Chat Completions",
  endpointDisplay: "gateway.example", credentialSource: "environment", readiness: "ready",
}];

function dialog() {
  const onChoose = vi.fn();
  const onCancel = vi.fn();
  const returnFocusRef = createRef<HTMLSelectElement>();
  const content = (available: ProviderConnection[]) => <LocaleProvider>
    <CustomModelDialog connections={available} currentModel={{ connectionId: "custom-env", modelId: "local-model" }}
      returnFocusRef={returnFocusRef} onChoose={onChoose} onCancel={onCancel} />
  </LocaleProvider>;
  const view = render(content(connections));
  return { onChoose, onCancel, replaceConnections: (available: ProviderConnection[]) => view.rerender(content(available)) };
}

it("explains invalid model IDs inline and cannot submit them", async () => {
  const user = userEvent.setup();
  const { onChoose } = dialog();
  const field = screen.getByLabelText("Model ID");
  await user.clear(field);
  expect(screen.getByText("Enter a model ID.")).toBeTruthy();
  expect((screen.getByRole("button", { name: "Use model" }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.submit(field.closest("form")!);
  expect(onChoose).not.toHaveBeenCalled();
  fireEvent.change(field, { target: { value: "model\u202e" } });
  expect(screen.getByText("Remove invisible control or direction characters from the model ID.")).toBeTruthy();
  expect(field.getAttribute("aria-invalid")).toBe("true");
  fireEvent.submit(field.closest("form")!);
  expect(onChoose).not.toHaveBeenCalled();
  fireEvent.change(field, { target: { value: "模型".repeat(100) } });
  expect(screen.getByText("Shorten the model ID to 256 bytes or fewer.")).toBeTruthy();
});

it("refuses a connection removed while the dialog is open without choosing another one", () => {
  const { onChoose, replaceConnections } = dialog();
  replaceConnections([{ ...connections[0], id: "sibling", label: "Sibling" }]);
  expect(screen.getByText("Choose a connection that is still configured.")).toBeTruthy();
  expect((screen.getByRole("button", { name: "Use model" }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.submit(screen.getByLabelText("Model ID").closest("form")!);
  expect(onChoose).not.toHaveBeenCalled();
});

it("does not choose or close the dialog while the model ID is being composed", async () => {
  const { onChoose, onCancel } = dialog();
  const field = screen.getByLabelText("Model ID");
  field.focus();
  fireEvent.compositionStart(field);
  expect(fireEvent.keyDown(field, { key: "Enter", isComposing: true, keyCode: 229 })).toBe(false);
  fireEvent.submit(field.closest("form")!);
  expect(fireEvent.keyDown(field, { key: "Escape", isComposing: true })).toBe(false);
  expect(onChoose).not.toHaveBeenCalled();
  expect(onCancel).not.toHaveBeenCalled();
  fireEvent.compositionEnd(field);
  expect(fireEvent.keyDown(field, { key: "Enter" })).toBe(false);
  fireEvent.keyUp(field, { key: "Enter" });
  await userEvent.setup().click(screen.getByRole("button", { name: "Use model" }));
  expect(onChoose).toHaveBeenCalledExactlyOnceWith({ connectionId: "custom-env", modelId: "local-model" });
});
