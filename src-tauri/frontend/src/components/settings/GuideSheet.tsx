import { useEffect, useId, useRef, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useFocusTrap } from "../../lib/hooks";

export type GuideId =
  | "plan-suggestion"
  | "providers"
  | "subagents-pool"
  | "image-understanding";
/** 手册页脚动作：由宿主场景解释（手册组件本身不碰路由与页签状态）。 */
export type GuideAction = "open-request-audit" | "open-image-understanding";

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
      <h3><span className="idx">04</span>两个 DeepSeek 开关，各管各的</h3>
      <p>
        <strong>「复杂任务先建议制定计划」</strong>只控制是否主动询问：开启后 DeepSeek
        在识别到复杂任务时问你一次；关闭则全部直接执行，手动 Plan 模式不受影响。
        按任务<strong>实际使用的服务</strong>生效，无需把 DeepSeek 设为默认服务。
      </p>
      <p>
        <strong>「DeepSeek Plan 锚定」</strong>控制进入 DeepSeek Plan 之后的行为：
        开启时规划期只保留必要的只读工具和上下文，你批准实施后自动恢复该任务全部
        可用能力；关闭时 Plan 使用标准目录与上下文。两者互不替代——可以只开建议
        不开锚定，也可以关掉建议但手动进入 Plan 时仍用锚定。锚定开关只影响之后
        创建的计划；正在进行的计划在创建时已固定设置。
      </p>
      <p>
        若线上出现异常弹窗，维护者可通过环境变量 <code>R_CODE_PLANNING_EMERGENCY_OFF=1</code>
        一键全局暂停建议与锚定；暂停期间手动 Plan 模式及其只读安全边界不受影响。
      </p>
    </section>
  </>
);

const PROVIDERS_BODY = (
  <>
    <section>
      <h3><span className="idx">01</span>服务与默认服务</h3>
      <p>
        「服务」是一条可用的模型线路配置（厂商、地址、协议、密钥、模型），可以配置
        多条并存。<strong>默认服务</strong>只决定<strong>新对话</strong>使用哪条线路；
        已开始的对话不受影响，每个对话还可以单独切换服务与模型。
      </p>
    </section>
    <section>
      <h3><span className="idx">02</span>协议与线路要一起选</h3>
      <p>
        同一厂商经常同时提供多种接口（Anthropic 口、OpenAI Chat、OpenAI Responses），
        计费与能力各不相同。切换「接口线路」时协议会一起切换；自建网关地址请按其实际
        实现选择协议，选错会得到 404 或请求被拒绝。
      </p>
    </section>
    <section>
      <h3><span className="idx">03</span>密钥存储在哪里</h3>
      <p>
        访问密钥只保存在当前设备的安全凭据存储中（macOS 本地加密文件、其他平台系统
        凭据库），配置文件与界面都不会回显已保存内容，也不会随项目分发。
      </p>
    </section>
    <section>
      <h3><span className="idx">04</span>模型同步与多模态标注</h3>
      <p>
        <strong>已保存的服务会在点开时自动同步模型清单</strong>（五分钟内重复点开不重复
        请求），结果保存在本机供模型选择与图片理解使用；手动「同步模型」只在
        <strong>新建服务</strong>填完密钥后需要。候选后面的 <strong>[多模态]</strong> /
        <strong>[文本]</strong> 标注来自人工核对的预设目录：多模态模型可直接接收图片，
        文本模型不支持图片输入；未标注的模型能力未确认。
      </p>
    </section>
  </>
);

const SUBAGENTS_POOL_BODY = (
  <>
    <section>
      <h3><span className="idx">01</span>候选来源</h3>
      <p>
        子代理槽位可以从两类来源中选择：已配置的 API Provider（按服务配置的模型计费）
        和本机登录的 Codex CLI。同一来源可以重复出现在多个槽位，配不同模型与用途。
      </p>
    </section>
    <section>
      <h3><span className="idx">02</span>权重必须合计 100%</h3>
      <p>
        自动委派子代理时按槽位权重比例分流。全部槽位的权重合计必须等于 100% 才能保存；
        只有一个槽位时它就是 100%。
      </p>
    </section>
    <section>
      <h3><span className="idx">03</span>连通回执与保存规则</h3>
      <p>
        每个槽位按「来源 + 模型」精确测试连通性，成功回执约 30 分钟内有效、失败回执
        5 分钟后可重测。保存要求<strong>全部槽位当前连通</strong>（all-or-nothing）：
        任一槽位未通过测试都会被拒绝，不会保存半套配置。
      </p>
    </section>
    <section>
      <h3><span className="idx">04</span>自动测试</h3>
      <p>
        进入本页会自动测试「配置就绪但尚未连通」的候选来源与已保存槽位（一分钟内重复
        进入不重复请求），结果会显示在面板顶部的状态行。失败项不会弹错误打断你，
        可稍后手动点「测试连接」重测。
      </p>
    </section>
  </>
);

const IMAGE_UNDERSTANDING_BODY = (
  <>
    <section>
      <h3><span className="idx">01</span>多模态主模型不需要辅助</h3>
      <p>
        <strong>主模型本身支持图片输入（目录确认多模态，如 Claude、GPT、豆包、
        GLM-4.6V、Qwen-VL 系列）时，原图直接发送给主模型</strong>，不经过本机 OCR、
        也不经过视觉模型——辅助引擎只服务<strong>文本主模型</strong>（DeepSeek V4、
        代码模型等），避免本末倒置。附件标签会显示「多模态直发」。
      </p>
    </section>
    <section>
      <h3><span className="idx">02</span>两种辅助引擎怎么选</h3>
      <p>
        <strong>本机 OCR（默认）</strong>：离线、免费，用系统自带的文字识别把图片里的
        文字提取成文本后发给模型，只提取文字，适合截图报错、日志、代码片段。
        <strong>视觉模型</strong>：由你指定的多模态模型阅读整张图片并生成结构化描述
        （界面元素、图表、布局等非文字信息也能覆盖），多张图片并发理解，每次消耗该
        服务的调用。
      </p>
    </section>
    <section>
      <h3><span className="idx">03</span>发送时的等待是什么</h3>
      <p>
        文本主模型带图发送时，会在对话开始前先完成图片转换（输入区会显示「正在理解
        图片…」）——转换完成后对话自动开始。切到多模态主模型后原图直发，没有这段
        等待。切换只影响之后新发送的图片；原图仅本地留存供预览回看。
      </p>
    </section>
    <section>
      <h3><span className="idx">04</span>失败降级链</h3>
      <p>
        视觉模型引擎失败（超时、限流、余额不足）时：Windows/macOS 上 PNG/JPEG 图片会
        自动降级为本机 OCR 并在文本中标注降级原因；无法降级时（如 Linux 无系统 OCR
        或 GIF/WebP）发送会返回明确错误并列出失败图片，不会静默丢弃。
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
  providers: {
    eyebrow: "模型服务指引",
    title: "模型服务、默认服务与线路",
    intro: "理解服务配置、默认服务的语义，以及协议与多模态标注。",
    body: PROVIDERS_BODY,
    footNote: "内容随应用版本内置；Esc 随时关闭。",
  },
  "subagents-pool": {
    eyebrow: "子代理指引",
    title: "子代理候选池与连通测试",
    intro: "槽位、权重与连通回执的规则一览。",
    body: SUBAGENTS_POOL_BODY,
    footNote: "内容随应用版本内置；Esc 随时关闭。",
  },
  "image-understanding": {
    eyebrow: "图片理解指引",
    title: "图片理解引擎：本机 OCR 与视觉模型",
    intro: "图片如何被转成模型可读的内容，以及失败时会发生什么。",
    body: IMAGE_UNDERSTANDING_BODY,
    footNote: "内容随应用版本内置；Esc 随时关闭。",
    action: { id: "open-image-understanding", label: "去配置图片理解" },
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
