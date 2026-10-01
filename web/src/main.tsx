// Import order matters: logging, then token capture, then legacy URL redirect.
import "./logging-init";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./lib/token";
import "./lib/legacySessionRedirect";
import { BrowserRouter } from "react-router-dom";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { ToastBusBridge, ToastProvider } from "./components/Toasts";
import { installFetchErrorToasts } from "./lib/fetchInterceptor";
import { sweepAcpStateStorage } from "./hooks/useAcpSession";
import "./index.css";

if ("serviceWorker" in navigator) {
  navigator.serviceWorker.register("/sw.js");
}

installFetchErrorToasts();

// Before first render, and regardless of whether a session is ever opened:
// drop expired structured view badge entries and every pre-#4021 frozen
// `aoe:acp-state:v1:` snapshot, which can be megabytes and fails every other
// localStorage write while it sits there.
sweepAcpStateStorage();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ErrorBoundary>
      <ToastProvider>
        <ToastBusBridge />
        <BrowserRouter>
          <App />
        </BrowserRouter>
      </ToastProvider>
    </ErrorBoundary>
  </StrictMode>,
);
