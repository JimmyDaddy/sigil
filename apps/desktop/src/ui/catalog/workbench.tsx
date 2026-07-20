import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "../../App";
import type { ThemePreference } from "../../appearance/contract";
import "../../styles.css";
import { createCatalogWorkbenchBridge } from "./workbenchBridge";

const root = document.getElementById("workbench-root");
if (root === null) throw new Error("desktop workbench fixture root element is missing");

const candidate = new URLSearchParams(window.location.search).get("theme");
const theme: ThemePreference = candidate === "light" || candidate === "dark"
  ? candidate
  : "system";

createRoot(root).render(
  <StrictMode>
    <App bridge={createCatalogWorkbenchBridge(theme)} />
  </StrictMode>,
);
