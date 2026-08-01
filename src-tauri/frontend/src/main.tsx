import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { ErrorBoundary } from "./components/ui/ErrorBoundary";
import { CodexCliGateProvider } from "./components/codex/CodexCliGate";
import "./styles/tokens.css";
import "./styles/base.css";
import "./styles/components.css";
import "./styles/markdown.css";
import "./styles/shell.css";
import "./styles/scenes.css";
import "./styles/r-code-ui.css";
import "./styles/product-ui.css";
import "./styles/workbench.css";
import "./styles/signature.css";
import "./styles/onboarding.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary>
      {/* 全局门禁让所有 Codex 入口共享同一套安装与登录确认。 */}
      <CodexCliGateProvider>
        <App />
      </CodexCliGateProvider>
    </ErrorBoundary>
  </React.StrictMode>
);
