import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  subagentPoolSave,
  subagentPoolSnapshot,
  subagentProviderTest,
  subagentProviderTestBatch,
} from "../../lib/ipc";
import { providerIconFor, providerInitial } from "../../lib/provider-icons";
import { InfoTip } from "../ui/InfoTip";
import type {
  SubagentPoolSnapshot,
  SubagentPoolSlotHealth,
  SubagentProviderCatalogEntry,
  SubagentProviderHealthState,
  SubagentProviderProbeRequest,
  SubagentProviderSlot,
  SubagentProviderSource,
} from "../../lib/types";

const MAX_SLOTS = 3;
const MAX_PROMPT_CHARS = 12_000;
const WEIGHT_STEP = 5;

const PROMPT_TEMPLATES = [
  {
    id: "implementation",
    label: "功能实现",
    prompt: "实现委派的功能事项，严格遵守给定边界；完成后返回变更摘要、关键文件和可复现的验证证据。",
  },
  {
    id: "test_verification",
    label: "测试验证",
    prompt: "独立验证委派结果，优先运行最小而有区分度的测试；报告通过项、失败项、复现条件和仍未覆盖的风险。",
  },
  {
    id: "technical_research",
    label: "技术调研",
    prompt: "围绕委派问题进行证据优先的技术调研，比较可行方案、兼容性与安全边界，并给出可执行建议和来源。",
  },
  {
    id: "code_review",
    label: "代码评审",
    prompt: "审查委派范围内的实现，优先发现正确性、回归、安全和可维护性问题；按严重度给出带文件位置的结论。",
  },
] as const;

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function sourceKey(source: SubagentProviderSource): string {
  return source.kind === "api_provider" ? `api:${source.provider_id}` : "codex_cli";
}

function candidateKey(source: SubagentProviderSource, model: string): string {
  return `${sourceKey(source)} ${model}`;
}

function sameSource(left: SubagentProviderSource, right: SubagentProviderSource): boolean {
  return sourceKey(left) === sourceKey(right);
}

function healthLabel(state: SubagentProviderHealthState): string {
  switch (state) {
    case "connected": return "已连通";
    case "failed": return "连接失败";
    case "stale": return "需重新测试";
    case "untested": return "未测试";
  }
}

function availabilityLabel(entry: SubagentProviderCatalogEntry): string {
  switch (entry.availability) {
    case "ready": return "配置就绪";
    case "needs_configuration": return "需要补全配置";
    case "not_installed": return "尚未安装";
    case "login_required": return "需要登录";
    case "trust_required": return "需要重新确认安装来源";
    case "unsupported": return "当前不支持";
  }
}

function probeDetail(entry: SubagentProviderCatalogEntry): string {
  const verification = entry.health.verification_level === "remote_catalog"
    ? "远端目录验证"
    : entry.health.verification_level === "inference"
      ? "推理验证"
      : null;
  const latency = entry.health.latency_ms != null ? `${entry.health.latency_ms} ms` : null;
  const error = entry.health.error ? `错误：${entry.health.error}` : null;
  return [verification, latency, error].filter(Boolean).join(" · ") || availabilityLabel(entry);
}

function capabilityLabel(entry: SubagentProviderCatalogEntry): string {
  if (entry.capabilities.supports_host_delegation && entry.capabilities.supports_live_messages) {
    return "原生节点 · 可继续委派和接收运行中消息";
  }
  return "叶节点 · 不继续派生，不支持运行中消息";
}

function cloneSlots(slots: SubagentProviderSlot[]): SubagentProviderSlot[] {
  return slots.map((slot) => ({
    ...slot,
    source: { ...slot.source },
  }));
}

// 进入面板自动探测的节流窗口：窗口内的重复挂载/来源刷新不重发探测请求，
// 避免用户在设置页频繁切换时对 provider 造成重复连通性调用。
const AUTO_PROBE_THROTTLE_MS = 60_000;
let lastAutoProbeAt = 0;
/** 最近一次自动探测的汇总（模块级保存：节流窗口内重进面板显示“沿用”而非清空）。 */
let lastAutoProbeSummary: AutoProbeSummary | null = null;

export interface AutoProbeSummary {
  tested: number;
  connected: number;
  failed: number;
  /** true = 节流窗口内重复进入，未重新发请求。 */
  throttled: boolean;
}

/** 槽位在快照中的健康投影：按 slot_id + source + model 精确匹配。 */
export function slotHealthOf(
  slot: SubagentProviderSlot,
  snapshot: SubagentPoolSnapshot | null,
): SubagentPoolSlotHealth | undefined {
  return (snapshot?.slot_health ?? []).find((health) => (
    health.slot_id === slot.slot_id
    && sameSource(health.source, slot.source)
    && health.model === slot.model
  ));
}

/** B1：自动探测的请求列表 = 目录条目 ∪ 已保存槽位，按 (source, model) 去重。
 *
 * - 目录条目：配置就绪、有模型、未连通（receipt 已连通的跳过）；selectable 是
 *   连通测试的结果，不能作为首次测试的前置条件；
 * - 已保存槽位：有模型且当前槽位健康不是 connected——健康回执按 (source, model)
 *   精确键控，槽位使用非默认模型时目录条目测不到它，必须单独补测；
 * - 槽位对应来源未就绪（无密钥/未安装）时跳过，等配置补全后再测。 */
export function buildAutoProbeRequests(
  snapshot: SubagentPoolSnapshot | null,
): SubagentProviderProbeRequest[] {
  if (!snapshot) return [];
  const catalog = snapshot.catalog?.entries ?? [];
  const readySources = new Set(
    catalog.filter((entry) => entry.ready).map((entry) => sourceKey(entry.source)),
  );
  const seen = new Set<string>();
  const requests: SubagentProviderProbeRequest[] = [];
  const push = (source: SubagentProviderSource, model: string) => {
    if (!model.trim()) return;
    const key = candidateKey(source, model);
    if (seen.has(key)) return;
    seen.add(key);
    requests.push({ source: { ...source }, model });
  };
  for (const entry of catalog) {
    if (entry.ready && entry.model && entry.health.state !== "connected") {
      push(entry.source, entry.model);
    }
  }
  for (const slot of snapshot.pool?.slots ?? []) {
    const state = slotHealthOf(slot, snapshot)?.health.state ?? "untested";
    if (state === "connected") continue;
    if (!readySources.has(sourceKey(slot.source))) continue;
    push(slot.source, slot.model);
  }
  return requests;
}

/** B2：常驻状态行文案。失败不弹错误条（保持现状不打断配置），只计入状态行。 */
export function autoProbeStatusLabel(summary: AutoProbeSummary | null): string | null {
  if (!summary) return null;
  const outcome = `已自动测试 ${summary.tested} 项：${summary.connected} 项连通`
    + (summary.failed > 0 ? `，${summary.failed} 项失败（失败项可手动重测）` : "");
  return summary.throttled
    ? `一分钟内不重复测试：沿用最近的连通结果（${outcome}）。`
    : `本次进入${outcome}。`;
}

function newSlotId(sequence: number): string {
  return `subagent-slot-${Date.now().toString(36)}-${sequence}`;
}

export function SubagentProvidersPanel({
  providerKinds,
  refreshSignal,
  onOpenGuide,
}: {
  /** provider 档案名 → 目录 provider_kind，用于给候选来源匹配厂商图标。 */
  providerKinds?: Record<string, string | undefined>;
  /** 外部就绪信号（如 Codex 登录/协作状态变化）：递增时保留草稿、仅刷新来源目录。 */
  refreshSignal?: number;
  /** 打开指引手册（E2：子代理面板头部入口）。 */
  onOpenGuide?: (guideId: "subagents-pool") => void;
}) {
  const slotSequence = useRef(0);
  const [snapshot, setSnapshot] = useState<SubagentPoolSnapshot | null>(null);
  const [slots, setSlots] = useState<SubagentProviderSlot[]>([]);
  const [probeResults, setProbeResults] = useState<Record<string, SubagentProviderCatalogEntry>>({});
  const [autoProbe, setAutoProbe] = useState<AutoProbeSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [busyKeys, setBusyKeys] = useState<ReadonlySet<string>>(() => new Set());
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const isBusy = useCallback((key: string) => busyKeys.has(key), [busyKeys]);
  const setBusy = useCallback((key: string, busy: boolean) => {
    setBusyKeys((current) => {
      const next = new Set(current);
      if (busy) next.add(key);
      else next.delete(key);
      return next;
    });
  }, []);
  // 全局操作（保存/批量测试/重新加载/自动探测）会整体替换快照，互相及与单测互斥。
  const globalBusy = ["save", "batch", "reload", "auto-probe"].some((key) => busyKeys.has(key));
  // 单个来源测试彼此独立；全局操作需要等单测结束，避免快照互相覆盖。
  const anyTestBusy = Array.from(busyKeys).some((key) => key.startsWith("test:"));

  const load = useCallback(async (replaceDraft = true) => {
    setLoading(true);
    setBusy("reload", true);
    try {
      const next = await subagentPoolSnapshot();
      setSnapshot(next);
      if (replaceDraft) setSlots(cloneSlots(next?.pool?.slots ?? []));
      setProbeResults({});
      setError(null);
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setBusy("reload", false);
      setLoading(false);
    }
  }, [setBusy]);

  // 进入面板即自动探测：目录条目 + 已保存槽位合并去重，只补测"尚未连通"的
  // (source, model) 组合（已连通的沿用持久化 receipt）。模块级节流保证短时间内
  // 反复进出设置页或刷新信号触发重载时不会重复请求；手动"全部测试"不受节流影响。
  const autoProbeBusy = useRef(false);
  // 探测响应自带的快照会再次触发 snapshot effect；标记来源避免节流分支
  // 立即用"沿用"文案覆盖刚写好的本次结果。
  const probeSnapshotRef = useRef<SubagentPoolSnapshot | null>(null);
  const runAutoProbe = useCallback(async (next: SubagentPoolSnapshot) => {
    if (autoProbeBusy.current) return;
    if (probeSnapshotRef.current === next) return;
    const requests = buildAutoProbeRequests(next);
    if (requests.length === 0) return;
    const now = Date.now();
    if (now - lastAutoProbeAt < AUTO_PROBE_THROTTLE_MS) {
      if (lastAutoProbeSummary) setAutoProbe({ ...lastAutoProbeSummary, throttled: true });
      return;
    }
    lastAutoProbeAt = now;
    autoProbeBusy.current = true;
    setBusy("auto-probe", true);
    try {
      const response = await subagentProviderTestBatch(requests);
      probeSnapshotRef.current = response.snapshot;
      setSnapshot(response.snapshot);
      setProbeResults((current) => {
        const next = { ...current };
        response.results.forEach((entry) => {
          next[candidateKey(entry.source, entry.model)] = entry;
        });
        return next;
      });
      const connected = response.results.filter((entry) => entry.health.state === "connected").length;
      const summary: AutoProbeSummary = {
        tested: response.results.length,
        connected,
        failed: response.results.length - connected,
        throttled: false,
      };
      lastAutoProbeSummary = summary;
      setAutoProbe(summary);
    } catch {
      // 自动探测失败不打断配置流程；用户仍可手动测试。
    } finally {
      autoProbeBusy.current = false;
      setBusy("auto-probe", false);
    }
  }, [setBusy]);

  useEffect(() => {
    void load();
  }, [load]);

  // 快照到达后触发一次自动探测（依赖 snapshot 而非 catalog，保证每次重载都评估）。
  useEffect(() => {
    if (!snapshot) return;
    void runAutoProbe(snapshot);
  }, [snapshot, runAutoProbe]);

  // Codex 协作状态推进（登录完成、协作配置完成）后，Host 侧候选目录的
  // codex_cli 条目会随之就绪；这里只刷新目录，不清掉用户正在编辑的槽位草稿。
  const firstSignal = useRef(true);
  useEffect(() => {
    if (firstSignal.current) {
      firstSignal.current = false;
      return;
    }
    void load(false);
  }, [refreshSignal, load]);

  const catalog = snapshot?.catalog?.entries ?? [];
  const weightTotal = useMemo(() => slots.reduce((sum, slot) => sum + slot.weight, 0), [slots]);
  const dirty = snapshot ? JSON.stringify(slots) !== JSON.stringify(snapshot.pool?.slots ?? []) : false;

  const iconForSource = useCallback((source: SubagentProviderSource): string | null => {
    const kind = source.kind === "api_provider" ? providerKinds?.[source.provider_id] : "codex_cli";
    return providerIconFor(kind);
  }, [providerKinds]);

  const entryForSource = useCallback((source: SubagentProviderSource) => (
    catalog.find((entry) => sameSource(entry.source, source)) ?? null
  ), [catalog]);

  const entryForSlot = useCallback((slot: SubagentProviderSlot) => {
    const tested = probeResults[candidateKey(slot.source, slot.model)];
    if (tested) return tested;
    const direct = catalog.find((entry) => sameSource(entry.source, slot.source) && entry.model === slot.model);
    if (direct) return direct;
    const persistedHealth = slotHealthOf(slot, snapshot);
    const base = entryForSource(slot.source);
    if (!base || !persistedHealth) return null;
    return {
      ...base,
      model: slot.model,
      connected: persistedHealth.health.state === "connected",
      selectable: persistedHealth.selectable,
      availability: persistedHealth.availability,
      capabilities: persistedHealth.capabilities,
      health: persistedHealth.health,
    };
  }, [catalog, entryForSource, probeResults, snapshot]);

  const validation = useMemo(() => {
    const issues: string[] = [];
    if (slots.length > MAX_SLOTS) issues.push(`最多只能配置 ${MAX_SLOTS} 个槽位。`);
    if (new Set(slots.map((slot) => slot.slot_id)).size !== slots.length) issues.push("槽位标识必须唯一。");
    slots.forEach((slot, index) => {
      const label = `槽位 ${index + 1}`;
      if (!entryForSource(slot.source)) issues.push(`${label} 的来源已被删除。`);
      if (!slot.model || slot.model.trim() !== slot.model) issues.push(`${label} 需要填写无首尾空格的模型。`);
      if (!Number.isInteger(slot.weight) || slot.weight < 1 || slot.weight > 100) {
        issues.push(`${label} 的权重必须是 1 到 100 的整数。`);
      }
      if (!slot.prompt.trim()) issues.push(`${label} 的 Prompt 不能为空。`);
      if ([...slot.prompt].length > MAX_PROMPT_CHARS) issues.push(`${label} 的 Prompt 超过 ${MAX_PROMPT_CHARS} 字符。`);
      const entry = entryForSlot(slot);
      if (!entry?.selectable || entry.health.state !== "connected") {
        issues.push(`${label} 的来源与模型尚未通过当前配置下的连通测试。`);
      }
    });
    if (slots.length > 0 && weightTotal !== 100) issues.push(`权重合计必须为 100%，当前为 ${weightTotal}%。`);
    return [...new Set(issues)];
  }, [entryForSlot, entryForSource, slots, weightTotal]);

  const updateSlot = (slotId: string, update: (slot: SubagentProviderSlot) => SubagentProviderSlot) => {
    setSlots((current) => current.map((slot) => slot.slot_id === slotId ? update(slot) : slot));
    setNotice(null);
  };

  const addSlot = () => {
    const source = catalog.find((entry) => entry.selectable);
    if (!source || slots.length >= MAX_SLOTS) return;
    slotSequence.current += 1;
    const template = PROMPT_TEMPLATES[0];
    setSlots((current) => {
      // 新槽位默认拿剩余权重：第一个槽位即 100%，通常一步就能保存。
      const used = current.reduce((sum, slot) => sum + slot.weight, 0);
      return [...current, {
        slot_id: newSlotId(slotSequence.current),
        source: { ...source.source },
        model: source.model,
        weight: Math.min(100, Math.max(1, 100 - used)),
        prompt_template_id: template.id,
        prompt: template.prompt,
      }];
    });
    setNotice(null);
    setError(null);
  };

  const applyProbeResults = (
    nextSnapshot: SubagentPoolSnapshot,
    results: SubagentProviderCatalogEntry[],
  ) => {
    setSnapshot(nextSnapshot);
    setProbeResults((current) => {
      const next = { ...current };
      results.forEach((entry) => {
        next[candidateKey(entry.source, entry.model)] = entry;
      });
      return next;
    });
  };

  const testOne = async (request: SubagentProviderProbeRequest) => {
    if (globalBusy) return;
    const key = candidateKey(request.source, request.model);
    const busyToken = `test:${key}`;
    setBusy(busyToken, true);
    setError(null);
    setNotice(null);
    try {
      const response = await subagentProviderTest(request);
      applyProbeResults(response.snapshot, [response.result]);
      setNotice(`${response.result.display_name} · ${response.result.model}：${healthLabel(response.result.health.state)}。`);
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setBusy(busyToken, false);
    }
  };

  const testAll = async () => {
    if (globalBusy || anyTestBusy) return;
    const requests: SubagentProviderProbeRequest[] = [];
    const seen = new Set<string>();
    [...catalog.map((entry) => ({ source: entry.source, model: entry.model })), ...slots.map((slot) => ({ source: slot.source, model: slot.model }))]
      .forEach((request) => {
        if (!request.model.trim()) return;
        const key = candidateKey(request.source, request.model);
        if (seen.has(key)) return;
        seen.add(key);
        requests.push(request);
      });
    if (requests.length === 0) return;
    setBusy("batch", true);
    setError(null);
    setNotice("正在逐项测试已配置来源…");
    try {
      const response = await subagentProviderTestBatch(requests);
      applyProbeResults(response.snapshot, response.results);
      const connected = response.results.filter((entry) => entry.health.state === "connected").length;
      setNotice(`批量测试完成：${connected}/${response.results.length} 项已连通；失败项不会进入候选池。`);
    } catch (cause) {
      setError(errorText(cause));
      setNotice(null);
    } finally {
      setBusy("batch", false);
    }
  };

  const save = async () => {
    if (!snapshot || validation.length > 0 || !dirty || globalBusy || anyTestBusy) return;
    setBusy("save", true);
    setError(null);
    setNotice(null);
    try {
      const next = await subagentPoolSave(snapshot.revision, { slots: cloneSlots(slots) });
      setSnapshot(next);
      setSlots(cloneSlots(next?.pool?.slots ?? []));
      setProbeResults({});
      setNotice((next?.pool?.slots?.length ?? 0) > 0 ? "子代理候选池已原子保存。" : "候选池已清空，将继续使用原有委派路由。 ");
    } catch (cause) {
      const message = errorText(cause);
      if (/revision|其他窗口|已更新|冲突/i.test(message)) {
        await load(true);
        setNotice("配置已在其他窗口变化，已重新加载最新候选池。");
      } else {
        setError(message);
      }
    } finally {
      setBusy("save", false);
    }
  };

  const weightHint = useMemo(() => {
    if (slots.length === 0) return null;
    if (weightTotal < 100) return `权重还差 ${100 - weightTotal}%，用 ＋ 补齐到 100% 后即可保存。`;
    if (weightTotal > 100) return `权重超出 ${weightTotal - 100}%，用 − 调低到 100% 后即可保存。`;
    return null;
  }, [slots.length, weightTotal]);

  return (
    <section className="settings-block subagent-providers-panel" id="subagent-pool-block" aria-labelledby="subagent-providers-title">
      <header className="subagent-providers-heading">
        <div>
          <h3 id="subagent-providers-title">候选来源与路由池</h3>
          <p className="desc">从已配置的 API Provider 与 Codex CLI 组成最多 3 个槽位；同一来源可以重复使用。</p>
        </div>
        <div className="subagent-provider-actions">
          {onOpenGuide && (
            <button
              type="button"
              className="guide-link"
              aria-haspopup="dialog"
              onClick={() => onOpenGuide("subagents-pool")}
            >
              指引手册 <span aria-hidden="true">→</span>
            </button>
          )}
          <button className="btn sm" disabled={loading || globalBusy || anyTestBusy || catalog.length === 0} onClick={() => void testAll()}>
            {isBusy("batch") ? "正在批量测试…" : "全部测试"}
          </button>
          <button className="btn sm ghost" disabled={loading || globalBusy || anyTestBusy} onClick={() => void load(true)}>重新加载</button>
        </div>
      </header>

      <details className="subagent-provider-boundary">
        <summary>连通性与保存规则</summary>
        <p>进入本页会自动探测配置就绪但尚未连通的来源与已保存槽位（一分钟内重复进入不会重复请求）。失败、过期或配置已变化的来源会保持不可选；保存时 Host 会再次复核。</p>
      </details>
      {autoProbeStatusLabel(autoProbe) && (
        <p className="subagent-autoprobe-status" role="status" aria-live="polite" data-testid="subagent-autoprobe-status">
          {autoProbeStatusLabel(autoProbe)}
        </p>
      )}
      {loading && !snapshot && <div className="settings-loading">正在读取子代理候选来源…</div>}
      {error && <div className="errbar" role="alert">{error}</div>}
      {notice && <div className="okbar" role="status" aria-live="polite">{notice}</div>}

      {snapshot && (
        <>
          <div className="subagent-source-heading">
            <h4>候选来源</h4>
            <span className="subagent-source-count">{catalog.length} 项</span>
          </div>
          <div className="subagent-provider-catalog" data-testid="subagent-provider-catalog">
            {catalog.map((entry) => {
              const key = candidateKey(entry.source, entry.model);
              const current = probeResults[key] ?? entry;
              const icon = iconForSource(current.source);
              return (
                <article className="subagent-provider-row" key={key} data-source-key={sourceKey(entry.source)}>
                  <span className={`provider-icon-tile${icon ? "" : " is-fallback"}`} aria-hidden="true">
                    {icon ? <img src={icon} alt="" /> : providerInitial(current.display_name)}
                  </span>
                  <div className="subagent-provider-copy">
                    <div className="subagent-provider-name">
                      <strong>{current.display_name}</strong>
                      <span className="subagent-provider-model">{current.model || "未配置模型"}</span>
                    </div>
                    <small>{probeDetail(current)}</small>
                    <small>{capabilityLabel(current)}</small>
                  </div>
                  <span className={`subagent-status is-${current.health.state}`}>{healthLabel(current.health.state)}</span>
                  <button
                    className="btn sm"
                    disabled={globalBusy || isBusy(`test:${key}`) || !current.ready || !current.model}
                    onClick={() => void testOne({ source: current.source, model: current.model })}
                  >
                    {isBusy(`test:${key}`) ? "测试中…" : "测试连接"}
                  </button>
                </article>
              );
            })}
          </div>

          <div className="subagent-pool-heading">
            <div>
              <h4>候选槽位</h4>
              <p>每个槽位绑定一个来源和一个权重；全部槽位权重合计等于 100% 时，候选池才能启用。</p>
            </div>
            <div className="subagent-pool-summary">
              <span className={slots.length > 0 && weightTotal !== 100 ? "invalid" : ""}>权重 {weightTotal}%</span>
              <span>{slots.length}/{MAX_SLOTS} 槽</span>
              <button
                className="btn sm"
                data-testid="subagent-add-slot"
                disabled={globalBusy || slots.length >= MAX_SLOTS || !catalog.some((entry) => entry.selectable)}
                onClick={addSlot}
              >
                添加槽位
              </button>
            </div>
          </div>

          {slots.length === 0 && (
            <div className="subagent-pool-empty">
              <div className="subagent-pool-empty-copy">
                <strong>还没有槽位</strong>
                <p>
                  槽位决定自动委派子代理时按什么比例把任务分给不同来源。
                  例如：槽位 1 选一个来源、权重 100%，表示所有子代理任务都走这个来源。
                </p>
                <p className="subagent-pool-empty-hint">尚未启用候选池；新的自动委派继续使用现有路由策略。</p>
              </div>
              <button
                className="btn sm"
                disabled={globalBusy || !catalog.some((entry) => entry.selectable)}
                onClick={addSlot}
              >
                ＋ 添加槽位
              </button>
            </div>
          )}

          <div className="subagent-slot-list">
            {slots.map((slot, index) => {
              const selectedSource = entryForSource(slot.source);
              const testedEntry = entryForSlot(slot);
              const selectedTemplateKnown = PROMPT_TEMPLATES.some((template) => template.id === slot.prompt_template_id);
              return (
                <article className="subagent-slot-card" key={slot.slot_id} data-testid="subagent-slot-card">
                  <header>
                    <div>
                      <strong>槽位 {index + 1}</strong>
                      <span className={`subagent-status is-${testedEntry?.health.state ?? "untested"}`}>
                        {testedEntry ? healthLabel(testedEntry.health.state) : selectedSource ? "未测试此模型" : "来源已删除"}
                      </span>
                    </div>
                    <div className="subagent-slot-actions">
                      <button
                        className="quiet-link danger"
                        disabled={globalBusy}
                        aria-label={`删除槽位 ${index + 1}`}
                        onClick={() => setSlots((current) => current.filter((candidate) => candidate.slot_id !== slot.slot_id))}
                      >
                        删除
                      </button>
                    </div>
                  </header>

                  <div className="subagent-slot-grid">
                    <label>
                      <span>来源</span>
                      <select
                        className="input"
                        aria-label={`槽位 ${index + 1} 来源`}
                        value={sourceKey(slot.source)}
                        disabled={isBusy("save")}
                        onChange={(event) => {
                          const entry = catalog.find((candidate) => sourceKey(candidate.source) === event.target.value);
                          if (!entry?.selectable) return;
                          updateSlot(slot.slot_id, (current) => ({ ...current, source: { ...entry.source }, model: entry.model }));
                        }}
                      >
                        {!selectedSource && <option value={sourceKey(slot.source)} disabled>来源已删除</option>}
                        {catalog.map((entry) => (
                          <option key={sourceKey(entry.source)} value={sourceKey(entry.source)} disabled={!entry.selectable}>
                            {entry.display_name} · {healthLabel(entry.health.state)}
                          </option>
                        ))}
                      </select>
                    </label>
                    <label>
                      <span>模型</span>
                      <input
                        className="input"
                        aria-label={`槽位 ${index + 1} 模型`}
                        value={slot.model}
                        maxLength={320}
                        disabled={isBusy("save")}
                        onChange={(event) => updateSlot(slot.slot_id, (current) => ({ ...current, model: event.target.value }))}
                      />
                    </label>
                    <label>
                      <span>权重 <InfoTip label="权重说明">自动委派子代理时按槽位权重比例分流；全部槽位合计必须等于 100% 才能保存。</InfoTip></span>
                      <div className="subagent-weight-input">
                        <button
                          type="button"
                          className="subagent-weight-step"
                          aria-label={`槽位 ${index + 1} 减少权重`}
                          disabled={isBusy("save") || slot.weight <= 1}
                          onClick={() => updateSlot(slot.slot_id, (current) => ({
                            ...current,
                            weight: Math.max(1, current.weight - WEIGHT_STEP),
                          }))}
                        >
                          −
                        </button>
                        <input
                          className="input"
                          type="number"
                          min={1}
                          max={100}
                          step={1}
                          aria-label={`槽位 ${index + 1} 权重`}
                          value={slot.weight}
                          disabled={isBusy("save")}
                          onChange={(event) => updateSlot(slot.slot_id, (current) => ({ ...current, weight: Number(event.target.value) }))}
                        />
                        <button
                          type="button"
                          className="subagent-weight-step"
                          aria-label={`槽位 ${index + 1} 增加权重`}
                          disabled={isBusy("save") || slot.weight >= 100}
                          onClick={() => updateSlot(slot.slot_id, (current) => ({
                            ...current,
                            weight: Math.min(100, current.weight + WEIGHT_STEP),
                          }))}
                        >
                          ＋
                        </button>
                        <span>%</span>
                      </div>
                    </label>
                  </div>

                  <label className="subagent-template-field">
                    <span>Prompt 模板</span>
                    <select
                      className="input"
                      aria-label={`槽位 ${index + 1} Prompt 模板`}
                      value={slot.prompt_template_id ?? "custom"}
                      disabled={isBusy("save")}
                      onChange={(event) => {
                        const template = PROMPT_TEMPLATES.find((candidate) => candidate.id === event.target.value);
                        updateSlot(slot.slot_id, (current) => template
                          ? { ...current, prompt_template_id: template.id, prompt: template.prompt }
                          : { ...current, prompt_template_id: null });
                      }}
                    >
                      <option value="custom">自定义 Prompt</option>
                      {!selectedTemplateKnown && slot.prompt_template_id && (
                        <option value={slot.prompt_template_id}>{slot.prompt_template_id}（当前模板）</option>
                      )}
                      {PROMPT_TEMPLATES.map((template) => <option key={template.id} value={template.id}>{template.label}</option>)}
                    </select>
                  </label>
                  <label className="subagent-prompt-field">
                    <span>最终 Prompt <small>{[...slot.prompt].length}/{MAX_PROMPT_CHARS}</small></span>
                    <textarea
                      className="input"
                      aria-label={`槽位 ${index + 1} 最终 Prompt`}
                      rows={4}
                      maxLength={MAX_PROMPT_CHARS}
                      value={slot.prompt}
                      disabled={isBusy("save")}
                      onChange={(event) => updateSlot(slot.slot_id, (current) => ({ ...current, prompt: event.target.value }))}
                    />
                  </label>
                  {testedEntry && <p className="subagent-slot-capability">{capabilityLabel(testedEntry)}</p>}
                </article>
              );
            })}
          </div>

          {slots.length > 0 && (
            <div className="subagent-weight-bar-wrap">
              <div className="subagent-weight-bar" role="img" aria-label={`权重分布，合计 ${weightTotal}%`}>
                {slots.map((slot, index) => (
                  <span
                    key={slot.slot_id}
                    className={`subagent-weight-seg seg-${index % 3}`}
                    style={{ width: `${Math.max(0, Math.min(100, slot.weight))}%` }}
                  />
                ))}
              </div>
              {weightHint && <p className="subagent-weight-hint">{weightHint}</p>}
            </div>
          )}

          <footer className="subagent-pool-footer">
            <div className="subagent-validation" aria-live="polite">
              {validation.length > 0
                ? <ul>{validation.map((issue) => <li key={issue}>{issue}</li>)}</ul>
                : <span>{slots.length > 0 ? "候选池可保存；Host 会再次校验健康 receipt。" : "空候选池会保留原有委派路由。"}</span>}
            </div>
            <button
              className="btn primary"
              data-testid="subagent-save-pool"
              disabled={isBusy("save") || validation.length > 0 || !dirty}
              onClick={() => void save()}
            >
              {isBusy("save") ? "正在保存…" : "保存候选池"}
            </button>
          </footer>
        </>
      )}
    </section>
  );
}
