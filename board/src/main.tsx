import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./app";
import { installCursors } from "./cursors";
import "./styles.css";

installCursors();

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}
