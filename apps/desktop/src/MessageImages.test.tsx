import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { Message } from "./Message";
import { LocaleProvider } from "./i18n";

afterEach(() => { cleanup(); window.localStorage.clear(); });
const image = { attachmentId: "image-1", mimeType: "image/png" as const, width: 2, height: 3, byteLen: 72 };
it("opens a recovered image using exact message and attachment identity", async () => {
  const read = vi.fn(async () => "data:image/png;base64,aW1hZ2U=");
  render(<LocaleProvider><Message displayId="display-1" message={{ key: "message-1", kind: "user", label: "You", text: "Describe this", images: [image] }} onReadImage={read} /></LocaleProvider>);
  expect(read).not.toHaveBeenCalled();
  await userEvent.setup().click(screen.getByRole("button", { name: "View image" }));
  expect(read).toHaveBeenCalledWith("display-1", "image-1");
  expect((await screen.findByRole("img")).getAttribute("src")).toBe("data:image/png;base64,aW1hZ2U=");
});
it("preserves the transcript and explains a missing or changed saved image", async () => {
  render(<LocaleProvider><Message displayId="display-1" message={{ key: "message-1", kind: "user", label: "You", text: "Describe this", images: [image] }} onReadImage={async () => { throw new Error("unavailable"); }} /></LocaleProvider>);
  await userEvent.setup().click(screen.getByRole("button", { name: "View image" }));
  await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("missing or changed"));
  expect(screen.getByText("Describe this")).toBeTruthy();
});
