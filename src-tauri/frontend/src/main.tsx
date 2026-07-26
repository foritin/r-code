import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { ErrorBoundary } from "./components/ui/ErrorBoundary";
import "./styles/tokens.css";
import "./styles/base.css";
import "./styles/components.css";
import "./styles/markdown.css";
import "./styles/shell.css";
import "./styles/scenes.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </React.StrictMode>
);
