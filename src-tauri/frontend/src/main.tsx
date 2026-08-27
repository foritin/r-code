import React from "react";
import ReactDOM from "react-dom/client";
import "./i18n";
import App from "./App";
import { ErrorBoundary } from "./components/ui/ErrorBoundary";
import { CodexCliGateProvider } from "./components/codex/CodexCliGate";
import { CompanionWindow } from "./components/companion/CompanionWindow";
import {
  NativeCompanionWindowController,
  prepareNativeCompanionWindow,
} from "./components/companion/CompanionWindowController";
import "./styles/tokens.css";
import "./styles/base.css";
import "./styles/components.css";
import "./styles/markdown.css";
import "./styles/shell.css";
import "./styles/scenes.css";
import "./styles/r-code-ui.css";
import "./styles/product-ui.css";
import "./styles/memory.css";
import "./styles/workbench.css";
import "./styles/signature.css";
import "./styles/onboarding.css";
import "./styles/companion.css";

const isCompanionWindow = new URLSearchParams(window.location.search).get("window") === "companion";
if (isCompanionWindow) prepareNativeCompanionWindow();

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary>
      {isCompanionWindow ? (
        <>
          <NativeCompanionWindowController />
          <CompanionWindow />
        </>
      ) : (
        /* 全局门禁只属于主窗口；独立 companion 不启动完整 App 副作用。 */
        <CodexCliGateProvider>
          <App />
        </CodexCliGateProvider>
      )}
    </ErrorBoundary>
  </React.StrictMode>
);
