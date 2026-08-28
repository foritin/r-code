// M3-01/M3-02：关闭确认对话框（Host 权威的“问一问”渲染面）。
// Host gate 发出 close-prompt-request {epoch}；本组件是唯一渲染者，
// 决定回传 cmd_close_prompt_decision，真实 hide/quit 由 Host 执行。

import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  CLOSE_PROMPT_REQUEST_EVENT,
  closePromptDecision,
} from "../../lib/ipc";

interface PromptPayload {
  epoch: number;
}

export default function ClosePromptDialog() {
  const [epoch, setEpoch] = useState<number | null>(null);
  const [remember, setRemember] = useState(false);
  const [busy, setBusy] = useState(false);
  const dialogRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    // 纯浏览器（无 Tauri internals）没有事件桥：浏览器 mock 下不会有关闭 prompt。
    if (typeof window === "undefined" || !Reflect.has(window, "__TAURI_INTERNALS__")) {
      return;
    }
    const unlisten = listen<PromptPayload>(CLOSE_PROMPT_REQUEST_EVENT, (event) => {
      setEpoch(event.payload.epoch);
      setRemember(false);
      setBusy(false);
      setTimeout(() => dialogRef.current?.focus(), 30);
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  if (epoch === null) return null;

  const decide = (decision: "hide" | "quit" | "cancel") => {
    if (busy) return;
    setBusy(true);
    void closePromptDecision(epoch, decision, remember)
      .catch(() => {})
      .finally(() => {
        // Host 执行 hide/quit 后窗口可能直接消失；cancel/失败由事件重开。
        setEpoch(null);
        setBusy(false);
      });
  };

  return (
    <div
      className="settings-modal-scrim"
      role="presentation"
      onClick={(event) => {
        if (event.target === event.currentTarget) decide("cancel");
      }}
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="close-prompt-title"
        tabIndex={-1}
        className="settings-modal"
        onKeyDown={(event) => {
          if (event.key === "Escape") decide("cancel");
        }}
      >
        <h2 id="close-prompt-title">要关闭 R-Code 吗？</h2>
        <p className="settings-modal-body">
          可以最小化到后台继续运行任务，或完全退出应用。运行中的任务在退出前会收到统一收尾。
        </p>
        <label className="settings-control-row">
          <input
            type="checkbox"
            checked={remember}
            onChange={(event) => setRemember(event.target.checked)}
          />
          记住我的选择，下次不再询问
        </label>
        <div className="settings-modal-actions">
          <button type="button" className="btn" disabled={busy} onClick={() => decide("cancel")}>
            取消
          </button>
          <button type="button" className="btn" disabled={busy} onClick={() => decide("hide")}>
            后台运行
          </button>
          <button
            type="button"
            className="btn danger"
            disabled={busy}
            onClick={() => decide("quit")}
          >
            退出
          </button>
        </div>
      </div>
    </div>
  );
}
