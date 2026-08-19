import { useEffect, useId, useRef, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useFocusTrap } from "../../lib/hooks";

/** 「首轮工具清单」档位的唯一事实来源：设置页下拉与指引手册的档位卡都从这里取，
 * 手册文案与 `orchestration.first_round_catalog` 枚举值不会各自漂移。 */
export const FIRST_ROUND_CATALOG_OPTIONS = [
  { value: "full", label: "完整清单 · 不锚定（默认）" },
  { value: "readonly", label: "只读清单 · 读文件/搜索等" },
  { value: "editor_pair", label: "读写最小对 · read_file + edit" },
] as const;

export const FIRST_ROUND_PROMOTE_OPTIONS = [
  { value: "either", label: "任意首轮回应（默认）" },
  { value: "tool_call", label: "首次真实工具调用" },
] as const;

export type GuideId = "first-round-catalog";
/** 手册页脚动作：由宿主场景解释（手册组件本身不碰路由与页签状态）。 */
export type GuideAction = "open-request-audit";

interface GuideEntry {
  eyebrow: string;
  title: string;
  intro: string;
  body: ReactNode;
  footNote: string;
  action?: { id: GuideAction; label: string };
}

const CATALOG_TIER_META = {
  full: {
    tag: "默认 · 不锚定",
    recommended: false,
    menu: ["read_file", "edit", "bash", "search"],
    more: true,
    use: "完全现状。随时退回，不产生任何行为差异。",
  },
  readonly: {
    tag: "推荐起步",
    recommended: true,
    menu: ["read_file", "list_files", "search", "glob"],
    more: false,
    use: "逼模型首轮先侦察再动手。适合大多数“先理解再改”的开发任务。",
  },
  editor_pair: {
    tag: "最激进",
    recommended: false,
    menu: ["read_file", "edit"],
    more: false,
    use: "首轮只有读+改的闭环。适合边界清晰的小改动；需要跑命令验证的任务会拖慢起步。",
  },
} as const;

const FIRST_ROUND_CATALOG_BODY = (
  <>
    <section>
      <h3><span className="idx">01</span>这是什么</h3>
      <p>
        每次发给模型的请求都携带一份「工具清单」（<code>tools</code> 数组：工具名 + 参数说明）。
        模型只能从清单里选择工具——清单就是它的菜单，与你的项目文件夹无关。
      </p>
      <p>
        清单在会话中途变化（托管联网工具启停、收尾总结轮清空等）会带来两个成本：
        其一，清单排在对话历史之前，字节级前缀缓存的公共前缀断在清单处，历史要重新计费重算；
        其二，第一轮给几十个工具，选择面过大会让模型在还没理解代码时就动用重工具。
        锚定把清单变化收敛为受控的一次：首轮收窄 → 晋升后恢复完整 → 会话内不再变。
      </p>
    </section>

    <section>
      <h3><span className="idx">02</span>三个档位</h3>
      <div className="tier-grid">
        {FIRST_ROUND_CATALOG_OPTIONS.map((option) => {
          const meta = CATALOG_TIER_META[option.value];
          return (
            <div
              key={option.value}
              className={`tier-card${meta.recommended ? " is-recommended" : ""}`}
            >
              <span className="tier-name">{option.label.split(" ·")[0]}</span>
              <span className="tier-tag">{meta.tag}</span>
              <div className="tier-menu">
                {meta.menu.map((tool) => <span key={tool} className="menu-chip">{tool}</span>)}
                {meta.more && <span className="menu-chip more">…全部</span>}
              </div>
              <p className="tier-use">{meta.use}</p>
            </div>
          );
        })}
      </div>
    </section>

    <section>
      <h3><span className="idx">03</span>恢复完整清单的时机</h3>
      <div className="promote-rows">
        <div className="promote-row">
          <span className="promote-name">{FIRST_ROUND_PROMOTE_OPTIONS[0].label.replace("（默认）", "")}</span>
          <div>
            <p className="promote-desc">模型给出任意 assistant 回复（含纯文本）即恢复完整清单。</p>
            <span className="promote-note">默认。首轮收窄很快结束，清单变化几乎无感。</span>
          </div>
        </div>
        <div className="promote-row">
          <span className="promote-name">{FIRST_ROUND_PROMOTE_OPTIONS[1].label}</span>
          <div>
            <p className="promote-desc">模型必须真正发起工具调用才解除收窄；纯文本寒暄不触发。</p>
            <span className="promote-note">更严格：防止“好的我来做”式空谈提前放开清单。</span>
          </div>
        </div>
      </div>
    </section>

    <section>
      <h3><span className="idx">04</span>推荐组合</h3>
      <div className="combo-list">
        <div className="combo-row is-main">
          <span className="combo-pill">只读清单 + 任意回应</span>
          <div className="combo-body">
            <strong>最平衡，建议从这里开始</strong>
            <p>首轮只能读/搜/列目录，模型一开口就放开全部工具。适合大多数先理解再动手的任务。</p>
          </div>
        </div>
        <div className="combo-row">
          <span className="combo-pill">读写最小对 + 工具调用</span>
          <div className="combo-body">
            <strong>最激进，边界清晰的小改动</strong>
            <p>首轮只有 read_file + edit，且必须真正动手才恢复。</p>
            <p className="caveat">注意：需要跑命令验证的任务会被拖慢——首轮无法执行任何命令。</p>
          </div>
        </div>
        <div className="combo-row">
          <span className="combo-pill">完整清单</span>
          <div className="combo-body">
            <strong>随时退回</strong>
            <p>选择“完整清单 · 不锚定”即回到现状，运行中的会话本就不受影响。</p>
          </div>
        </div>
      </div>
    </section>

    <section>
      <h3><span className="idx">05</span>如何验证效果</h3>
      <ol className="verify-steps">
        <li>到「设置 → 诊断」打开<strong>请求构成审计</strong>开关。</li>
        <li>用锚定配置新开会话，跑 2–3 个真实任务。</li>
        <li>
          打开审计文件，对比第 1 轮与第 2 轮的 <code>tool_names</code> 字段：
          首轮应是收窄名单，晋升后恢复完整，之后不再变化。<br />
          <span className="path-chip">应用数据目录 / sessions / request-audit / {"{会话id}"}.jsonl</span>
        </li>
        <li>积累一周数据后，比较锚定开/关会话的清单种类数（= 缓存断点预算），再决定是否转正。</li>
      </ol>
    </section>

    <section>
      <h3><span className="idx">06</span>边界与事实</h3>
      <ul className="fact-list">
        <li>清单裁剪只是呈现层：模型看不见清单外的工具，但工具执行与审批边界原样工作，收窄不等于降低安全要求。</li>
        <li>只对新会话生效；运行中的会话不受配置变化影响，也不会被中途换清单。</li>
        <li>每个会话至多一次清单切换（收窄 → 完整），不存在反复横跳。</li>
        <li>「只读清单」是清单级收窄，与 Ask 模式的只读策略无关；Ask 模式本来就是只读，不受锚定影响。</li>
      </ul>
    </section>
  </>
);

/** 实验功能的随版本内置手册：离线可用、与配置行为同源维护。新实验在这里登记即可复用同一浮层壳。 */
export const GUIDE_ENTRIES: Record<GuideId, GuideEntry> = {
  "first-round-catalog": {
    eyebrow: "实验指引",
    title: "首轮工具清单锚定",
    intro: "目标是稳定首轮请求形状，而不是永久隐藏工具；配置写入即对新会话生效。",
    body: FIRST_ROUND_CATALOG_BODY,
    footNote: "内容随应用版本内置，与配置行为同源维护；Esc 随时关闭。",
    action: { id: "open-request-audit", label: "去开启请求构成审计" },
  },
};

interface Props {
  guideId: GuideId | null;
  onClose: () => void;
  onAction: (action: GuideAction) => void;
}

/** 指引手册浮层：初始焦点落在关闭按钮，Esc / 点击背板退出并把焦点还给触发按钮。
 * 与 ConfirmDialog 共用 portal + useFocusTrap 惯例。 */
export function GuideSheet({ guideId, onClose, onAction }: Props) {
  const entry = guideId ? GUIDE_ENTRIES[guideId] : null;
  const titleId = useId();
  const dialogRef = useRef<HTMLDivElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);
  const returnFocusRef = useRef<HTMLElement | null>(null);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;
  useFocusTrap(dialogRef, entry !== null);

  useEffect(() => {
    if (!entry) return;
    returnFocusRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    closeRef.current?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onCloseRef.current();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      const target = returnFocusRef.current;
      if (target && document.contains(target)) target.focus({ preventScroll: true });
    };
  }, [entry]);

  if (!entry) return null;

  return createPortal(
    <div
      className="guide-overlay"
      onPointerDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        ref={dialogRef}
        className="guide-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
      >
        <div className="guide-head">
          <div>
            <span className="eyebrow">{entry.eyebrow}</span>
            <h2 id={titleId}>{entry.title}</h2>
            <p>{entry.intro}</p>
          </div>
          <button
            ref={closeRef}
            type="button"
            className="guide-close"
            aria-label="关闭指引手册"
            onClick={onClose}
          >
            ×
          </button>
        </div>
        <div className="guide-body">{entry.body}</div>
        <div className="guide-foot">
          <p className="foot-note">{entry.footNote}</p>
          <span className="spacer" />
          {entry.action && (
            <button
              type="button"
              className="btn"
              onClick={() => onAction(entry.action!.id)}
            >
              {entry.action.label}
            </button>
          )}
          <button type="button" className="btn accent" onClick={onClose}>知道了</button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
