import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  subagentPoolSave,
  subagentPoolSnapshot,
  subagentProviderTest,
  subagentProviderTestBatch,
} from "../../lib/ipc";
import type {
  SubagentPoolSnapshot,
  SubagentProviderCatalogEntry,
  SubagentProviderHealthState,
  SubagentProviderProbeRequest,
  SubagentProviderSlot,
  SubagentProviderSource,
} from "../../lib/types";

const MAX_SLOTS = 3;
const MAX_PROMPT_CHARS = 12_000;

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
  return `${sourceKey(source)}\u0000${model}`;
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

function newSlotId(sequence: number): string {
  return `subagent-slot-${Date.now().toString(36)}-${sequence}`;
}

export function SubagentProvidersPanel() {
  const slotSequence = useRef(0);
  const [snapshot, setSnapshot] = useState<SubagentPoolSnapshot | null>(null);
  const [slots, setSlots] = useState<SubagentProviderSlot[]>([]);
  const [probeResults, setProbeResults] = useState<Record<string, SubagentProviderCatalogEntry>>({});
  const [loading, setLoading] = useState(true);
  const [busyKey, setBusyKey] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const load = useCallback(async (replaceDraft = true) => {
    setLoading(true);
    try {
      const next = await subagentPoolSnapshot();
      setSnapshot(next);
      if (replaceDraft) setSlots(cloneSlots(next.pool.slots));
      setProbeResults({});
      setError(null);
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const catalog = snapshot?.catalog.entries ?? [];
  const weightTotal = useMemo(() => slots.reduce((sum, slot) => sum + slot.weight, 0), [slots]);
  const dirty = snapshot ? JSON.stringify(slots) !== JSON.stringify(snapshot.pool.slots) : false;

  const entryForSource = useCallback((source: SubagentProviderSource) => (
    catalog.find((entry) => sameSource(entry.source, source)) ?? null
  ), [catalog]);

  const entryForSlot = useCallback((slot: SubagentProviderSlot) => {
    const tested = probeResults[candidateKey(slot.source, slot.model)];
    if (tested) return tested;
    const direct = catalog.find((entry) => sameSource(entry.source, slot.source) && entry.model === slot.model);
    if (direct) return direct;
    const persistedHealth = snapshot?.slot_health.find((health) => (
      health.slot_id === slot.slot_id
      && sameSource(health.source, slot.source)
      && health.model === slot.model
    ));
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
    setSlots((current) => [...current, {
      slot_id: newSlotId(slotSequence.current),
      source: { ...source.source },
      model: source.model,
      weight: current.length === 0 ? 100 : 1,
      prompt_template_id: template.id,
      prompt: template.prompt,
    }]);
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
    const key = candidateKey(request.source, request.model);
    setBusyKey(`test:${key}`);
    setError(null);
    setNotice(null);
    try {
      const response = await subagentProviderTest(request);
      applyProbeResults(response.snapshot, [response.result]);
      setNotice(`${response.result.display_name} · ${response.result.model}：${healthLabel(response.result.health.state)}。`);
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setBusyKey(null);
    }
  };

  const testAll = async () => {
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
    setBusyKey("batch");
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
      setBusyKey(null);
    }
  };

  const save = async () => {
    if (!snapshot || validation.length > 0 || !dirty) return;
    setBusyKey("save");
    setError(null);
    setNotice(null);
    try {
      const next = await subagentPoolSave(snapshot.revision, { slots: cloneSlots(slots) });
      setSnapshot(next);
      setSlots(cloneSlots(next.pool.slots));
      setProbeResults({});
      setNotice(next.pool.slots.length > 0 ? "子代理候选池已原子保存。" : "候选池已清空，将继续使用原有委派路由。 ");
    } catch (cause) {
      const message = errorText(cause);
      if (/revision|其他窗口|已更新|冲突/i.test(message)) {
        await load(true);
        setNotice("配置已在其他窗口变化，已重新加载最新候选池。");
      } else {
        setError(message);
      }
    } finally {
      setBusyKey(null);
    }
  };

  return (
    <section className="settings-block subagent-providers-panel" aria-labelledby="subagent-providers-title">
      <header className="subagent-providers-heading">
        <div>
          <h3 id="subagent-providers-title">候选来源与路由池</h3>
          <p className="desc">从已配置的 API Provider 与 Codex CLI 组成最多 3 个槽位；同一来源可以重复使用。</p>
        </div>
        <div className="subagent-provider-actions">
          <button className="btn sm" disabled={loading || busyKey != null || catalog.length === 0} onClick={() => void testAll()}>
            {busyKey === "batch" ? "正在批量测试…" : "全部测试"}
          </button>
          <button className="btn sm ghost" disabled={loading || busyKey != null} onClick={() => void load(true)}>重新加载</button>
        </div>
      </header>

      <div className="subagent-provider-boundary" role="note">
        连通测试只在点击后执行。未测试、失败、过期或配置已变化的来源会保持不可选；保存时 Host 会再次复核。
      </div>
      {loading && !snapshot && <div className="settings-loading">正在读取子代理候选来源…</div>}
      {error && <div className="errbar" role="alert">{error}</div>}
      {notice && <div className="okbar" role="status" aria-live="polite">{notice}</div>}

      {snapshot && (
        <>
          <div className="subagent-provider-catalog" data-testid="subagent-provider-catalog">
            {catalog.map((entry) => {
              const key = candidateKey(entry.source, entry.model);
              const current = probeResults[key] ?? entry;
              return (
                <article className="subagent-provider-row" key={key} data-source-key={sourceKey(entry.source)}>
                  <div className="subagent-provider-copy">
                    <div className="subagent-provider-name">
                      <strong>{current.display_name}</strong>
                      <span className={`subagent-health is-${current.health.state}`}>{healthLabel(current.health.state)}</span>
                    </div>
                    <span className="subagent-provider-model">{current.model || "未配置模型"}</span>
                    <small>{probeDetail(current)}</small>
                    <small>{capabilityLabel(current)}</small>
                  </div>
                  <button
                    className="btn sm"
                    disabled={busyKey != null || !current.ready || !current.model}
                    onClick={() => void testOne({ source: current.source, model: current.model })}
                  >
                    {busyKey === `test:${key}` ? "测试中…" : "测试连接"}
                  </button>
                </article>
              );
            })}
          </div>

          <div className="subagent-pool-heading">
            <div>
              <h4>候选槽位</h4>
              <p>权重使用正整数，启用候选池时合计必须严格等于 100%。</p>
            </div>
            <div className="subagent-pool-summary">
              <span className={slots.length > 0 && weightTotal !== 100 ? "invalid" : ""}>权重 {weightTotal}%</span>
              <span>{slots.length}/{MAX_SLOTS} 槽</span>
              <button
                className="btn sm"
                data-testid="subagent-add-slot"
                disabled={busyKey != null || slots.length >= MAX_SLOTS || !catalog.some((entry) => entry.selectable)}
                onClick={addSlot}
              >
                添加槽位
              </button>
            </div>
          </div>

          {slots.length === 0 && (
            <div className="subagent-pool-empty">尚未启用候选池；新的自动委派继续使用现有路由策略。</div>
          )}

          <div className="subagent-slot-list">
            {slots.map((slot, index) => {
              const selectedSource = entryForSource(slot.source);
              const testedEntry = entryForSlot(slot);
              const selectedTemplateKnown = PROMPT_TEMPLATES.some((template) => template.id === slot.prompt_template_id);
              const testKey = `test:${candidateKey(slot.source, slot.model)}`;
              return (
                <article className="subagent-slot-card" key={slot.slot_id} data-testid="subagent-slot-card">
                  <header>
                    <div>
                      <strong>槽位 {index + 1}</strong>
                      <span className={`subagent-health is-${testedEntry?.health.state ?? "untested"}`}>
                        {testedEntry ? healthLabel(testedEntry.health.state) : selectedSource ? "未测试此模型" : "来源已删除"}
                      </span>
                    </div>
                    <button
                      className="quiet-link danger"
                      disabled={busyKey != null}
                      aria-label={`删除槽位 ${index + 1}`}
                      onClick={() => setSlots((current) => current.filter((candidate) => candidate.slot_id !== slot.slot_id))}
                    >
                      删除
                    </button>
                  </header>

                  <div className="subagent-slot-grid">
                    <label>
                      <span>来源</span>
                      <select
                        className="input"
                        aria-label={`槽位 ${index + 1} 来源`}
                        value={sourceKey(slot.source)}
                        disabled={busyKey != null}
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
                        disabled={busyKey != null}
                        onChange={(event) => updateSlot(slot.slot_id, (current) => ({ ...current, model: event.target.value }))}
                      />
                    </label>
                    <label>
                      <span>权重</span>
                      <div className="subagent-weight-input">
                        <input
                          className="input"
                          type="number"
                          min={1}
                          max={100}
                          step={1}
                          aria-label={`槽位 ${index + 1} 权重`}
                          value={slot.weight}
                          disabled={busyKey != null}
                          onChange={(event) => updateSlot(slot.slot_id, (current) => ({ ...current, weight: Number(event.target.value) }))}
                        />
                        <span>%</span>
                      </div>
                    </label>
                    <button
                      className="btn sm subagent-slot-test"
                      disabled={busyKey != null || !selectedSource?.ready || !slot.model || slot.model.trim() !== slot.model}
                      onClick={() => void testOne({ source: slot.source, model: slot.model })}
                    >
                      {busyKey === testKey ? "测试中…" : "测试此槽"}
                    </button>
                  </div>

                  <label className="subagent-template-field">
                    <span>Prompt 模板</span>
                    <select
                      className="input"
                      aria-label={`槽位 ${index + 1} Prompt 模板`}
                      value={slot.prompt_template_id ?? "custom"}
                      disabled={busyKey != null}
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
                      disabled={busyKey != null}
                      onChange={(event) => updateSlot(slot.slot_id, (current) => ({ ...current, prompt: event.target.value }))}
                    />
                  </label>
                  {testedEntry && <p className="subagent-slot-capability">{capabilityLabel(testedEntry)}</p>}
                </article>
              );
            })}
          </div>

          <footer className="subagent-pool-footer">
            <div className="subagent-validation" aria-live="polite">
              {validation.length > 0
                ? <ul>{validation.map((issue) => <li key={issue}>{issue}</li>)}</ul>
                : <span>{slots.length > 0 ? "候选池可保存；Host 会再次校验健康 receipt。" : "空候选池会保留原有委派路由。"}</span>}
            </div>
            <button
              className="btn primary"
              data-testid="subagent-save-pool"
              disabled={busyKey != null || validation.length > 0 || !dirty}
              onClick={() => void save()}
            >
              {busyKey === "save" ? "正在保存…" : "保存候选池"}
            </button>
          </footer>
        </>
      )}
    </section>
  );
}
