import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { BootstrapApp } from "./BootstrapApp";
import "highlight.js/styles/github-dark.css";
import "katex/dist/katex.min.css";
import "./theme/tokens.css";
import "./theme/base.css";
import "./theme/workbench.css";

const root = document.getElementById("root");
if (root === null) {
  throw new Error("Missing application root");
}
createRoot(root).render(
  <StrictMode>
    <BootstrapApp />
  </StrictMode>
);
