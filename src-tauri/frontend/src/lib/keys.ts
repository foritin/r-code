/**
 * 全局快捷键：单一数据源。
 *
 * 原先的问题：
 * 1. 全局 handler 没有 isTypingTarget 守卫 —— 在 textarea 里按 Ctrl+K / Ctrl+E /
 *    Ctrl+N 会被劫持并 preventDefault，破坏原生文本编辑（macOS 上尤其明显）。
 * 2. 快捷键字符串在 MenuBar / Rail / Composer / SearchOverlay 等 8 处硬编码，
 *    且一律写死 "Ctrl"，在 macOS 上标签是错的。
 *
 * 现在：绑定与展示都从 KEYMAP 派生，平台标签由 modLabel() 统一给出。
 */
import { useEffect, useRef } from "react";

export type KeyAction =
  | "search"
  | "editor"
  | "new"
  | "settings"
  | "toggleRail"
  | "shortcuts"
  | "zoomIn"
  | "zoomOut"
  | "zoomReset"
  | "workbenchSummary"
  | "workbenchTerminal"
  | "workbenchFiles"
  | "workbenchReview";

interface KeyBinding {
  /** e.key.toLowerCase() 的匹配集合 */
  keys: string[];
  /** 展示用的键名（不含修饰键） */
  label: string;
  description: string;
  /** 是否需要 Ctrl/Cmd */
  mod: boolean;
  shift?: boolean;
  alt?: boolean;
}

export const KEYMAP: Record<KeyAction, KeyBinding> = {
  search: { keys: ["k"], label: "K", description: "搜索文件与内容", mod: true },
  editor: { keys: ["e"], label: "E", description: "打开编辑器", mod: true },
  new: { keys: ["n"], label: "N", description: "新建会话", mod: true },
  settings: { keys: [","], label: ",", description: "设置", mod: true },
  toggleRail: { keys: ["b"], label: "B", description: "折叠/展开侧栏", mod: true },
  shortcuts: { keys: ["/"], label: "/", description: "快捷键参考", mod: true },
  zoomIn: { keys: ["=", "+"], label: "+", description: "放大", mod: true },
  zoomOut: { keys: ["-"], label: "−", description: "缩小", mod: true },
  zoomReset: { keys: ["0"], label: "0", description: "重置缩放", mod: true },
  workbenchSummary: { keys: ["s"], label: "S", description: "运行与子代理", mod: true, alt: true },
  workbenchTerminal: { keys: ["`"], label: "`", description: "任务终端", mod: true },
  workbenchFiles: { keys: ["p"], label: "P", description: "任务文件", mod: true },
  workbenchReview: { keys: ["g"], label: "G", description: "审核变更", mod: true, shift: true },
};

const IS_MAC = typeof navigator !== "undefined" && /mac/i.test(navigator.platform || navigator.userAgent);

/** 修饰键的平台化标签：macOS 上是 ⌘，其余是 Ctrl。 */
export function modLabel(): string {
  return IS_MAC ? "⌘" : "Ctrl";
}

/** 供 UI 展示的完整快捷键文本，例如 "Ctrl K" / "⌘ K"。 */
export function keyLabel(action: KeyAction): string {
  const binding = KEYMAP[action];
  const parts: string[] = [];
  if (binding.mod) parts.push(modLabel());
  if (binding.alt) parts.push(IS_MAC ? "⌥" : "Alt");
  if (binding.shift) parts.push("Shift");
  parts.push(binding.label);
  return parts.join(" ");
}

export type GlobalKeyHandlers = Partial<Record<KeyAction, () => void>>;

export function useGlobalKeys(handlers: GlobalKeyHandlers): void {
  // handlers 每次渲染都是新对象；用 ref 保持最新引用，effect 只挂一次监听器。
  const ref = useRef(handlers);
  ref.current = handlers;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey)) return;
      // 缩放是窗口级操作，输入态下也允许；其余快捷键必须让位给原生文本编辑。
      const key = e.key.toLowerCase();
      const typing = isTypingTarget(e.target);

      for (const [action, binding] of Object.entries(KEYMAP) as [KeyAction, KeyBinding][]) {
        if (!binding.keys.includes(key)) continue;
        if (Boolean(binding.alt) !== e.altKey) continue;
        if (binding.shift && !e.shiftKey) continue;
        if (!binding.shift && e.shiftKey) continue;
        const isZoom = action === "zoomIn" || action === "zoomOut" || action === "zoomReset";
        if (typing && !isZoom) return;
        const handler = ref.current[action];
        if (!handler) return;
        e.preventDefault();
        handler();
        return;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
}

/** 判断事件是否发生在输入控件内（场景快捷键应忽略）。 */
export function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target.isContentEditable;
}

/**
 * 场景内键盘绑定的统一注册（原先 FleetRows / NeedsLane 都漏了依赖数组，
 * 每次渲染解绑重绑；Canvas 又各写一遍守卫）。
 */
export function useSceneKeys(
  map: Record<string, (e: KeyboardEvent) => void>,
  options: { enabled?: boolean; allowWhileTyping?: boolean } = {}
): void {
  const { enabled = true, allowWhileTyping = false } = options;
  const ref = useRef(map);
  ref.current = map;

  useEffect(() => {
    if (!enabled) return;
    const onKey = (e: KeyboardEvent) => {
      if (!allowWhileTyping && isTypingTarget(e.target)) return;
      const handler = ref.current[e.key] ?? ref.current[e.key.toLowerCase()];
      if (handler) handler(e);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [enabled, allowWhileTyping]);
}
