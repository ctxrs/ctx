import React from "react";
import ReactDOM from "react-dom/client";
import { applyContextTheme } from "@ctx/design/web";
import App from "./App";
import { initLoadTestTelemetry } from "./utils/loadTestTelemetry";
import { initMobileViewport } from "./utils/mobileViewport";
import { initPretextPerfDiagnostics } from "./utils/pretextPerfDiagnostics";
import { initWalRecorder } from "./utils/walRecorder";
import { initTheme } from "./utils/theme";
import { primeDaemonConnection } from "./api/client";
import { installGlobalRuntimeDiagnosticHandlers } from "./state/diagnosticsChannel";
import "@xterm/xterm/css/xterm.css";
import "./styles.css";

const primeAuthSession = () => {
  primeDaemonConnection();
};

primeAuthSession();
initMobileViewport();
initTheme();
applyContextTheme();
initLoadTestTelemetry();
initPretextPerfDiagnostics();
installGlobalRuntimeDiagnosticHandlers();
const wal = initWalRecorder();

const app = wal?.onRender ? (
  <React.Profiler id="App" onRender={wal.onRender}>
    <App />
  </React.Profiler>
) : (
  <App />
);

ReactDOM.createRoot(document.getElementById("root")!).render(<React.StrictMode>{app}</React.StrictMode>);
