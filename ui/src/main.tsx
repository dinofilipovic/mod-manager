import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { ToastProvider } from "./components/Toasts";
import "./styles.css";

// In a plain browser during development, fake the backend so the UI can be worked on.
if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)) {
  const { installDevMock } = await import("./devMock");
  installDevMock();
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ToastProvider>
      <App />
    </ToastProvider>
  </StrictMode>,
);
