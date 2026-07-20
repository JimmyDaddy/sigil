import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import "../../styles.css";
import "./catalog.css";
import { CatalogApp } from "./CatalogApp";

const root = document.getElementById("catalog-root");
if (root === null) throw new Error("desktop catalog root element is missing");

createRoot(root).render(<StrictMode><CatalogApp /></StrictMode>);
