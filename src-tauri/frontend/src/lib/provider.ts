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
import type { ProviderPreset, ProviderProtocol } from "./types";

export interface ProviderChoice {
  name: string;
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
let catalogPromise: Promise<void> | null = null;

export function loadCatalog(): Promise<void> {
  catalogPromise ??= providerCatalog()
    .then((catalog) => {
      catalogOrder = catalog.presets;
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
  return catalogById.get(name);
}

/** 全部预设，顺序即后端声明的展示顺序。目录未加载时为空数组。 */
export function catalogPresets(): ProviderPreset[] {
  return catalogOrder;
}

export function providerLabel(name: string): string {
  return catalogById.get(name)?.label ?? name;
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

/** 读取 provider 列表；`deps` 变化时重新拉取。 */
export function useProviders(deps: unknown[] = []): ProvidersState {
  const [choices, setChoices] = useState<ProviderChoice[]>([]);
  const [fallback, setFallback] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    setLoading(true);
    try {
      // 先等目录，否则首帧的展示名和候选模型会退化成裸 provider 名
      await loadCatalog();
      const response = await settingsGet();
      const custom = readCustom();
      const next = Object.entries(response.config.providers ?? {}).map(([name, config]) => {
        const model = config.model || "";
        const preset = presetOf(name);
        // 配置里的模型排最前，其后是预设候选，最后是用户手输过的
        const models = Array.from(
          new Set([model, ...(preset?.models ?? []), ...(custom[name] ?? [])].filter(Boolean))
        );
        return {
          name,
          label: providerLabel(name),
          model: model || preset?.model || name,
          models,
          ready: Boolean(response.provider_status?.[name]?.ready),
          protocol: response.provider_status?.[name]?.effective_protocol ?? config.protocol ?? preset?.protocol,
        };
      });
      setChoices(next);
      setFallback(response.config.default_provider ?? "");
      setError(null);
    } catch (cause) {
      setError(`读取模型服务失败：${errText(cause)}`);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
    // deps 由调用方给出（如 taskId / providerName），用于触发重新拉取
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);

  return { choices, fallback, loading, error, reload };
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
