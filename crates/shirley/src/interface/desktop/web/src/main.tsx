import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "@/App";
import { CloudAccessGate } from "@/components/CloudAccessGate";
import "@/styles/index.css";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <CloudAccessGate><App /></CloudAccessGate>
  </StrictMode>,
);
