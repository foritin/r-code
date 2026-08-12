/**
 * 模型服务（provider）与具体模型的共享逻辑。
 *
 * 原先 `ProviderChoice` 接口、`providerLabel()` 映射表、以及
 * "settingsGet() → 遍历 config.providers → 拼 ready 标志" 这段代码，
 * 在 HomeScene / RoomScene / Composer 里各抄了一份（前两处逐字相同）。
 */
import { useCallback, useEffect, useState } from "react";
import { providerCatalog, settingsGet } from "./ipc";
import { errText } from "./format";
import type { HostedWebRoute, ProviderPreset, ProviderProtocol } from "./types";
import { RUNTIME_SETTINGS_CHANGED_EVENT } from "./onboarding";

export interface ProviderChoice {
  name: string;
  /** Stable catalog/vendor identity; profile names and gateway URLs may be edited independently. */
  kind?: string;
  /** 展示名，例如 deepseek → DeepSeek */
  label: string;
  /** 设置里配置的默认模型 */
  model: string;
  /** 该服务下可选的模型（预设候选 + 配置值 + 用户自定义历史） */
  models: string[];
  ready: boolean;
  /** 实际请求协议，用于只展示该线路真正支持的模型参数。 */
  protocol?: ProviderProtocol;
}

/**
 * 内置服务目录（`provider_catalog.rs`）。
 *
 * 目录是编译期常量，进程内拉一次即可；`presets` 的 promise 缓存在模块作用域，
 * 并发调用共享同一次 IPC。拉失败不阻塞主流程——退回"名字即展示名"的旧行为。
 */
const catalogById = new Map<string, ProviderPreset>();
let catalogOrder: ProviderPreset[] = [];
let hostedWebRoutes: HostedWebRoute[] = [];
let catalogPromise: Promise<void> | null = null;

export function loadCatalog(): Promise<void> {
  catalogPromise ??= providerCatalog()
    .then((catalog) => {
      catalogOrder = catalog.presets;
      hostedWebRoutes = catalog.hosted_web_routes ?? [];
      for (const preset of catalog.presets) catalogById.set(preset.id, preset);
    })
    .catch(() => {
      // 旧版后端没有这条命令；下次调用重试
      catalogPromise = null;
    });
  return catalogPromise;
}

/** 目录里的预设，未加载或非内置服务时为 undefined。 */
export function presetOf(name: string): ProviderPreset | undefined {
  // 0.1.0 早期版本把 DeepSeek Anthropic 口存成独立 Provider；目录合并后
  // 继续把旧 key 映射到统一预设，编辑与模型选择都不会退化成“自建服务”。
  const canonical = name === "deepseek_anthropic" ? "deepseek" : name;
  return catalogById.get(canonical);
}

/** 全部预设，顺序即后端声明的展示顺序。目录未加载时为空数组。 */
export function catalogPresets(): ProviderPreset[] {
  return catalogOrder;
}

/** 已由后端接线的厂商托管联网线路；设置页据此展示当前表单的真实能力。 */
export function catalogHostedWebRoutes(): HostedWebRoute[] {
  return hostedWebRoutes;
}

export function providerLabel(name: string): string {
  return presetOf(name)?.label ?? name;
}

/**
 * 自定义模型名的本地记忆。
 *
 * hermes-config 的 ProviderConfig 只有单个 `model` 字段。内置服务的候选模型
 * 现在由 `provider_catalog.rs` 提供，但它是一份会过期的静态清单，也覆盖不到
 * 用户自建的网关——所以仍然记住用户实际用过的名字，两者合并展示。
 * 等后端接上厂商的 /v1/models 发现能力后，这一层可以退成纯兜底。
 */
const CUSTOM_KEY = "r-code.provider.models";

function readCustom(): Record<string, string[]> {
  try {
    const raw = window.localStorage.getItem(CUSTOM_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : {};
    return parsed && typeof parsed === "object" ? (parsed as Record<string, string[]>) : {};
  } catch {
    return {};
  }
}

export function rememberModel(providerName: string, model: string): void {
  const trimmed = model.trim();
  if (!trimmed) return;
  try {
    const all = readCustom();
    const list = all[providerName] ?? [];
    if (list.includes(trimmed)) return;
    all[providerName] = [...list, trimmed].slice(-12);
    window.localStorage.setItem(CUSTOM_KEY, JSON.stringify(all));
  } catch {
    /* 受限环境下不持久化，不影响本次使用 */
  }
}

export interface ProvidersState {
  choices: ProviderChoice[];
  /** 全局默认 provider 名 */
  fallback: string;
  loading: boolean;
  error: string | null;
  reload: () => Promise<void>;
}

interface ProviderSnapshot {
  choices: ProviderChoice[];
  fallback: string;
}

let providerSnapshot: ProviderSnapshot | null = null;
interface ProviderSnapshotRequest {
  generation: number;
  promise: Promise<ProviderSnapshot>;
}
let providerSnapshotGeneration = 0;
let providerSnapshotRequest: ProviderSnapshotRequest | null = null;

async function loadProviderSnapshot(): Promise<ProviderSnapshot> {
  // 先等目录，否则首帧的展示名和候选模型会退化成裸 provider 名。
  await loadCatalog();
  const response = await settingsGet();
  const custom = readCustom();
  const choices = Object.entries(response.config.providers ?? {}).map(([name, config]) => {
    const model = config.model || "";
    const preset = presetOf(config.provider_kind ?? name);
    // 配置里的模型排最前，其后是预设候选，最后是用户手输过的。
    const models = Array.from(
      new Set([model, ...(preset?.models ?? []), ...(custom[name] ?? [])].filter(Boolean))
    );
    return {
      name,
      kind: config.provider_kind,
      label: providerLabel(name),
      model: model || preset?.model || name,
      models,
      ready: Boolean(response.provider_status?.[name]?.ready),
      protocol: response.provider_status?.[name]?.effective_protocol ?? config.protocol ?? preset?.protocol,
    };
  });
  return {
    choices,
    fallback: response.config.default_provider ?? "",
  };
}

function requestProviderSnapshot(force = false): Promise<ProviderSnapshot> {
  if (!force && providerSnapshot) return Promise.resolve(providerSnapshot);
  if (providerSnapshotRequest?.generation === providerSnapshotGeneration) {
    return providerSnapshotRequest.promise;
  }
  const generation = providerSnapshotGeneration;
  const promise = loadProviderSnapshot()
    .then((snapshot) => {
      // A provider mutation may finish while this keychain read is in flight.
      // That response belongs to the old generation and must never replace the new snapshot.
      if (generation === providerSnapshotGeneration) providerSnapshot = snapshot;
      return snapshot;
    })
    .finally(() => {
      if (providerSnapshotRequest?.promise === promise) providerSnapshotRequest = null;
    });
  providerSnapshotRequest = { generation, promise };
  return promise;
}

/** Begin one settings generation before notifying every mounted hook consumer. */
function invalidateProviderSnapshot(): void {
  providerSnapshotGeneration += 1;
  providerSnapshot = null;
  providerSnapshotRequest = null;
}

// Provider IPC wrappers live in `ipc.ts`, so importing this module there would create a cycle.
// A single application-level listener advances the generation before component listeners reload.
if (typeof window !== "undefined") {
  window.addEventListener(RUNTIME_SETTINGS_CHANGED_EVENT, invalidateProviderSnapshot);
}

/** 读取 provider 列表；`deps` 变化时重新拉取。 */
export function useProviders(deps: unknown[] = []): ProvidersState {
  const [choices, setChoices] = useState<ProviderChoice[]>(() => providerSnapshot?.choices ?? []);
  const [fallback, setFallback] = useState(() => providerSnapshot?.fallback ?? "");
  const [loading, setLoading] = useState(() => providerSnapshot == null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async (force: boolean) => {
    // taskId 等依赖变化时会重新进入 effect，但全局快照仍然有效。缓存命中不切换
    // loading，避免会话切换时模型胶囊短暂闪成“读取中”。
    const cached = !force ? providerSnapshot : null;
    if (cached) {
      setChoices(cached.choices);
      setFallback(cached.fallback);
      setError(null);
      setLoading(false);
      return;
    }
    setLoading(true);
    const generation = providerSnapshotGeneration;
    try {
      const snapshot = await requestProviderSnapshot(force);
      if (generation !== providerSnapshotGeneration) return;
      setChoices(snapshot.choices);
      setFallback(snapshot.fallback);
      setError(null);
    } catch (cause) {
      setError(`读取模型服务失败：${errText(cause)}`);
    } finally {
      setLoading(false);
    }
  }, []);

  const reload = useCallback(async () => {
    setLoading(true);
    const generation = providerSnapshotGeneration;
    try {
      const snapshot = await requestProviderSnapshot(true);
      if (generation !== providerSnapshotGeneration) return;
      setChoices(snapshot.choices);
      setFallback(snapshot.fallback);
      setError(null);
    } catch (cause) {
      setError(`读取模型服务失败：${errText(cause)}`);
    } finally {
      setLoading(false);
    }
  }, []);

  const invalidateAndReload = useCallback(async () => {
    invalidateProviderSnapshot();
    await reload();
  }, [reload]);

  useEffect(() => {
    // 会话切换只需要复用同一份全局 Provider 快照；显式 reload 和设置变更事件
    // 才触发 OS 凭据状态重查。deps 仍用于让调用方在首个空快照时重试。
    void load(false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);

  useEffect(() => {
    const refresh = () => void reload();
    window.addEventListener(RUNTIME_SETTINGS_CHANGED_EVENT, refresh);
    return () => window.removeEventListener(RUNTIME_SETTINGS_CHANGED_EVENT, refresh);
  }, [reload]);

  return { choices, fallback, loading, error, reload: invalidateAndReload };
}

/** 会话当前生效的服务与模型（会话绑定优先，否则回退全局默认）。 */
export function resolveActive(
  choices: ProviderChoice[],
  fallback: string,
  boundProvider: string | null,
  boundModel: string | null
): { provider: ProviderChoice | undefined; name: string; model: string } {
  const name = boundProvider ?? fallback;
  const provider = choices.find((choice) => choice.name === name);
  return { provider, name, model: boundModel ?? provider?.model ?? "" };
}
