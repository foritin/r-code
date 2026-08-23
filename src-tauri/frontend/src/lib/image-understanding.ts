/**
 * 图片理解引擎的全局配置读取（docs/settings-ux-and-image-understanding.md D）。
 *
 * 默认引擎是本机 OCR；`model` 引擎下 chip 文案需要展示 `服务/模型` 标签。
 * 配置变化通过 RUNTIME_SETTINGS_CHANGED_EVENT 事件推进代际；会话内快照复用
 * （与 provider.ts 的全局快照同策略），切换会话不重复发起 IPC。
 */
import { useEffect, useState } from "react";
import { settingsGet } from "./ipc";
import { RUNTIME_SETTINGS_CHANGED_EVENT } from "./onboarding";
import type { ImageEngineInfo } from "../components/Attachments";

interface CachedEngine {
  generation: number;
  promise: Promise<ImageEngineInfo>;
}

let snapshot: ImageEngineInfo = { engine: "ocr", visionModelLabel: null };
let hasFetched = false;
let generation = 0;
let inflight: CachedEngine | null = null;

async function fetchEngine(): Promise<ImageEngineInfo | null> {
  try {
    const response = await settingsGet();
    const config = response.config.image_understanding;
    if (config?.engine === "model") {
      const provider = config.model_provider?.trim() || "";
      const model = config.model?.trim() || "";
      return {
        engine: "model",
        visionModelLabel: provider && model ? `${provider}/${model}` : null,
      };
    }
    return { engine: "ocr", visionModelLabel: null };
  } catch {
    // 读取失败不落缓存（返回 null）：下次挂载重试；发送时后端会做权威分派。
    return null;
  }
}

function requestEngine(force: boolean): Promise<ImageEngineInfo> {
  if (!force && hasFetched) return Promise.resolve(snapshot);
  if (inflight && inflight.generation === generation && !force) return inflight.promise;
  const currentGeneration = generation;
  const promise = fetchEngine().then((next) => {
    if (next && currentGeneration === generation) {
      snapshot = next;
      hasFetched = true;
    }
    return next ?? snapshot;
  }).finally(() => {
    // 只清理仍指向本次请求的槽位：强制重查可能已覆盖 inflight。
    if (inflight?.promise === promise) inflight = null;
  });
  inflight = { generation, promise };
  return promise;
}

function invalidateEngine(): void {
  generation += 1;
  inflight = null;
  // 快照标记必须一并失效，否则事件后挂载的消费者仍会拿到旧缓存
  //（与 provider.ts 的 invalidateProviderSnapshot 同策略）。
  hasFetched = false;
}

if (typeof window !== "undefined") {
  window.addEventListener(RUNTIME_SETTINGS_CHANGED_EVENT, invalidateEngine);
}

/** 读取当前图片理解引擎；设置变化事件自动失效重查。 */
export function useImageUnderstandingEngine(): ImageEngineInfo {
  const [engine, setEngine] = useState<ImageEngineInfo>(snapshot);

  useEffect(() => {
    let alive = true;
    const load = (force: boolean) => {
      void requestEngine(force).then((next) => {
        if (alive) setEngine(next);
      });
    };
    load(false);
    const refresh = () => load(true);
    window.addEventListener(RUNTIME_SETTINGS_CHANGED_EVENT, refresh);
    return () => {
      alive = false;
      window.removeEventListener(RUNTIME_SETTINGS_CHANGED_EVENT, refresh);
    };
  }, []);

  return engine;
}

/** 非组件场景的强制重查（设置保存后由事件驱动，一般无需手动调用）。 */
export function reloadImageUnderstandingEngine(): Promise<ImageEngineInfo> {
  return requestEngine(true);
}
