import { useEffect, useId, useRef, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useFocusTrap } from "../../lib/hooks";

export type GuideId = "plan-suggestion";
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

const PLAN_SUGGESTION_BODY = (
  <>
    <section>
      <h3><span className="idx">01</span>什么时候会建议先计划</h3>
      <p>
        处理复杂任务时（例如改动跨越多个相互依赖的部分、涉及数据或兼容性变化、需要你
        先拍板的方案取舍，或直接做错之后不好回退），R-Code 会问你一次：
        <strong>先列个计划，还是直接继续。</strong>每个任务最多问一次；选择直接继续后
        本任务不再主动弹出，你仍可随时手动选择 Plan 模式。
      </p>
    </section>
    <section>
      <h3><span className="idx">02</span>进入后会先调查，不会先改文件</h3>
      <p>
        Plan 模式只做只读调查：读文件、搜索、查看状态，必要时向你提几个阻塞性问题。
        在你看到并批准计划之前，不会修改任何文件、不会执行命令。
      </p>
    </section>
    <section>
      <h3><span className="idx">03</span>你需要再次批准才开始实施</h3>
      <p>
        计划列好后 R-Code 会展示完整清单：要做什么、按什么顺序、每一步怎么验证。
        你可以批准、继续追问或取消；批准后才开始实施，随时可以停下来。
      </p>
    </section>
    <section>
      <h3><span className="idx">04</span>首发只支持经过验证的 DeepSeek</h3>
      <p>
        自动建议目前只对通过验证的 DeepSeek 服务开启。其他模型服务不受影响：
        你仍可以随时手动选择 Plan 模式，功能与以往一致。
      </p>
    </section>
  </>
);

/** 实验功能的随版本内置手册：离线可用、与配置行为同源维护。新实验在这里登记即可复用同一浮层壳。 */
export const GUIDE_ENTRIES: Record<GuideId, GuideEntry> = {
  "plan-suggestion": {
    eyebrow: "Plan 模式指引",
    title: "Plan 模式与复杂任务建议",
    intro: "复杂任务开始修改前，先花十几秒确认范围和顺序。",
    body: PLAN_SUGGESTION_BODY,
    footNote: "内容随应用版本内置；Esc 随时关闭，不影响任何未做的决定。",
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
