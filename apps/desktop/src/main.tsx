import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./App";
import "./styles.css";

async function renderDesktop(): Promise<void> {
  if (import.meta.env.VITE_SIGIL_DESKTOP_E2E === "1") {
    await import("@wdio/tauri-plugin");
  }

  const root = document.getElementById("root");

  if (root === null) {
    throw new Error("desktop root element is missing");
  }

  createRoot(root).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

void renderDesktop();
