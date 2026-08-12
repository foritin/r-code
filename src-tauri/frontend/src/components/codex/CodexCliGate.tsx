import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { errText } from "../../lib/format";
import { useFocusTrap, useReturnFocus } from "../../lib/hooks";
import {
  codexInstallCli,
  codexIntegrationStatus,
  codexStartDeviceLogin,
  codexStartLogin,
} from "../../lib/ipc";
import type { CodexIntegrationStatus } from "../../lib/types";
import { IconAlert, IconCheck, IconClose, IconRefresh, IconTerminal } from "../icons";
import {
  CODEX_LOGIN_WAIT_MINUTES,
  nextCodexLoginPollDelay,
} from "./login-watcher";

const INSTALL_COMMAND = "npm install -g @openai/codex";
const STATUS_CACHE_MS = 15_000;

type GatePhase =
  | "confirm"
  | "installing"
  | "login"
  | "waiting-login"
  | "login-timeout"
  | "resuming"
  | "error";

export interface CodexCliRequirement {
  /** 面向用户描述本次点击要完成的动作。 */
  feature: string;
  /** 子代理必须先登录；终端和配置操作可由 Codex 自己继续引导。 */
  requireAuth?: boolean;
}

interface PendingRequest {
  requirement: CodexCliRequirement;
  action: () => Promise<void>;
  resolve: () => void;
  reject: (reason: unknown) => void;
}

interface CodexCliGateValue {
  runWithCodexCli: (requirement: CodexCliRequirement, action: () => Promise<void>) => Promise<void>;
}

const CodexCliGateContext = createContext<CodexCliGateValue | null>(null);

export function useCodexCliGate(): CodexCliGateValue {
  const value = useContext(CodexCliGateContext);
  if (!value) throw new Error("useCodexCliGate 必须在 CodexCliGateProvider 内使用");
  return value;
}

export function CodexCliGateProvider({ children }: { children: ReactNode }) {
  const [pending, setPending] = useState<PendingRequest | null>(null);
  const [status, setStatus] = useState<CodexIntegrationStatus | null>(null);
  const [phase, setPhase] = useState<GatePhase>("confirm");
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const dialogRef = useRef<HTMLDivElement>(null);
  const primaryRef = useRef<HTMLButtonElement>(null);
  const cachedStatusRef = useRef<CodexIntegrationStatus | null>(null);
  const checkedAtRef = useRef(0);
  const statusPromiseRef = useRef<Promise<CodexIntegrationStatus> | null>(null);
  const loginStartedAtRef = useRef(0);

  useFocusTrap(dialogRef, Boolean(pending));
  useReturnFocus(Boolean(pending));

  const rememberStatus = useCallback((next: CodexIntegrationStatus) => {
    cachedStatusRef.current = next;
    checkedAtRef.current = Date.now();
    setStatus(next);
    return next;
  }, []);

  const getStatus = useCallback(async (force = false) => {
    if (!force && cachedStatusRef.current && Date.now() - checkedAtRef.current < STATUS_CACHE_MS) {
      return cachedStatusRef.current;
    }
    // 手动检测与自动轮询共享同一个在途请求，避免重复启动 `codex login status`。
    if (statusPromiseRef.current) return statusPromiseRef.current;
    const request = codexIntegrationStatus(force)
      .then(rememberStatus)
      .finally(() => {
        statusPromiseRef.current = null;
      });
    statusPromiseRef.current = request;
    return request;
  }, [rememberStatus]);

  // 预热只读状态，使第一次点击 Codex 入口时通常可以立即给出结果。
  useEffect(() => {
    void getStatus().catch(() => {});
  }, [getStatus]);

  const close = useCallback((resolve = true) => {
    if (pending && resolve) pending.resolve();
    setPending(null);
    setError(null);
    setCopied(false);
    setPhase("confirm");
    loginStartedAtRef.current = 0;
  }, [pending]);

  const resume = useCallback(async (request: PendingRequest) => {
    setPhase("resuming");
    try {
      await request.action();
      request.resolve();
    } catch (reason) {
      request.reject(reason);
    } finally {
      setPending(null);
      setError(null);
      setCopied(false);
      setPhase("confirm");
      loginStartedAtRef.current = 0;
    }
  }, []);

  const runWithCodexCli = useCallback(async (
    requirement: CodexCliRequirement,
    action: () => Promise<void>,
  ) => {
    if (pending) throw new Error("另一个 Codex CLI 操作正在等待确认。");
    const cached = cachedStatusRef.current;
    const mustRefresh = cached?.cli_available === false
      || (requirement.requireAuth && cached?.auth_status === "not_authenticated");
    const current = await getStatus(mustRefresh);
    const needsLogin = requirement.requireAuth && current.auth_status === "not_authenticated";
    if (current.cli_available && !needsLogin) {
      await action();
      return;
    }

    await new Promise<void>((resolve, reject) => {
      setStatus(current);
      setError(null);
      setCopied(false);
      setPhase(current.cli_available ? "login" : "confirm");
      loginStartedAtRef.current = 0;
      setPending({ requirement, action, resolve, reject });
    });
  }, [getStatus, pending]);

  useEffect(() => {
    if (!pending) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && phase !== "installing" && phase !== "resuming") close();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [close, pending, phase]);

  useEffect(() => {
    if (!pending || phase === "installing" || phase === "resuming") return;
    const timer = window.setTimeout(() => primaryRef.current?.focus(), 0);
    return () => window.clearTimeout(timer);
  }, [pending, phase]);

  const continueRequest = useCallback(async (nextStatus: CodexIntegrationStatus) => {
    if (!pending) return;
    if (pending.requirement.requireAuth && nextStatus.auth_status === "not_authenticated") {
      setPhase("login");
      return;
    }
    await resume(pending);
  }, [pending, resume]);

  const install = useCallback(async () => {
    if (!pending) return;
    setPhase("installing");
    setError(null);
    try {
      const next = rememberStatus(await codexInstallCli());
      await continueRequest(next);
    } catch (reason) {
      setError(errText(reason));
      setPhase("error");
    }
  }, [continueRequest, pending, rememberStatus]);

  const beginLogin = useCallback(async (mode: "browser" | "device") => {
    setError(null);
    try {
      if (mode === "browser") await codexStartLogin();
      else await codexStartDeviceLogin();
      loginStartedAtRef.current = Date.now();
      setPhase("waiting-login");
    } catch (reason) {
      loginStartedAtRef.current = 0;
      setError(errText(reason));
      setPhase("login");
    }
  }, []);

  const checkLogin = useCallback(async () => {
    if (!pending) return;
    setError(null);
    try {
      const next = await getStatus(true);
      if (next.auth_status === "authenticated") await resume(pending);
      else setError("尚未检测到登录完成。请完成终端中的授权后再试。");
    } catch (reason) {
      setError(errText(reason));
    }
  }, [getStatus, pending, resume]);

  useEffect(() => {
    if (!pending || phase !== "waiting-login") return;
    let active = true;
    let timer: number | undefined;
    const startedAt = loginStartedAtRef.current || Date.now();
    loginStartedAtRef.current = startedAt;

    const expire = () => {
      if (!active) return;
      setError(null);
      setPhase("login-timeout");
    };
    const scheduleNext = () => {
      if (!active) return;
      const delay = nextCodexLoginPollDelay(startedAt);
      if (delay === null) {
        expire();
        return;
      }
      timer = window.setTimeout(() => void poll(), delay);
    };
    const poll = async () => {
      if (!active) return;
      try {
        const next = await getStatus(true);
        if (!active) return;
        if (next.auth_status === "authenticated") {
          await resume(pending);
          return;
        }
      } catch {
        // 临时探测失败不终止登录；在统一截止时间前继续尝试。
      }
      scheduleNext();
    };

    scheduleNext();
    return () => {
      active = false;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [getStatus, pending, phase, resume]);

  const copyCommand = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(status?.installer_command || INSTALL_COMMAND);
      setCopied(true);
    } catch {
      setError("无法写入剪贴板，请手动选择并复制命令。");
    }
  }, [status?.installer_command]);

  const value = useMemo<CodexCliGateValue>(() => ({ runWithCodexCli }), [runWithCodexCli]);
  const dismissible = phase !== "installing" && phase !== "resuming";

  return (
    <CodexCliGateContext.Provider value={value}>
      {children}
      {pending && createPortal(
        <div className="codex-gate-backdrop" onMouseDown={() => dismissible && close()}>
          <div
            className="codex-gate-dialog"
            role="dialog"
            aria-modal="true"
            aria-labelledby="codex-gate-title"
            aria-describedby="codex-gate-description"
            ref={dialogRef}
            onMouseDown={(event) => event.stopPropagation()}
          >
            <header className="codex-gate-head">
              <span className="codex-gate-mark" aria-hidden="true"><IconTerminal width={18} height={18} /></span>
              <div>
                <span>CODEX CLI</span>
                <h2 id="codex-gate-title">{gateTitle(phase)}</h2>
              </div>
              {dismissible && (
                <button className="codex-gate-close" onClick={() => close()} aria-label="关闭">
                  <IconClose width={16} height={16} />
                </button>
              )}
            </header>

            <div className="codex-gate-body">
              <p id="codex-gate-description">{gateDescription(phase, pending.requirement.feature)}</p>

              {(phase === "confirm" || phase === "error") && (
                <>
                  <div className="codex-install-command">
                    <code>{status?.installer_command || INSTALL_COMMAND}</code>
                    <button onClick={() => void copyCommand()}>{copied ? "已复制" : "复制"}</button>
                  </div>
                  <dl className="codex-install-facts">
                    <div><dt>来源</dt><dd>npm 当前 registry 的 @openai/codex</dd></div>
                    <div><dt>范围</dt><dd>npm 配置的全局安装目录</dd></div>
                    <div><dt>权限</dt><dd>不自动申请管理员权限</dd></div>
                    <div><dt>凭据</dt><dd>不读取 npm 或 Codex 登录信息</dd></div>
                  </dl>
                </>
              )}

              {phase === "installing" && (
                <div className="codex-gate-progress" role="status">
                  <span><i /></span>
                  <small>正在下载并安装官方 npm 包，通常需要 10–60 秒。</small>
                </div>
              )}

              {phase === "waiting-login" && (
                <div className="codex-gate-wait" role="status">
                  <IconRefresh width={15} height={15} />
                  <span>正在等待 Codex 返回登录状态；完成后会自动继续，最多等待 {CODEX_LOGIN_WAIT_MINUTES} 分钟。</span>
                </div>
              )}

              {phase === "login-timeout" && (
                <div className="codex-gate-wait timeout" role="status">
                  <IconAlert width={15} height={15} />
                  <span>等待已结束，但登录流程没有被取消。完成授权后可重新检测，也可重新打开浏览器或改用设备码。</span>
                </div>
              )}

              {phase === "resuming" && (
                <div className="codex-gate-wait" role="status">
                  <IconCheck width={15} height={15} />
                  <span>Codex CLI 已就绪，正在继续“{pending.requirement.feature}”。</span>
                </div>
              )}

              {(error || (phase === "confirm" && status?.installer_available === false)) && (
                <div className="codex-gate-error" role="alert">
                  <IconAlert width={15} height={15} />
                  <span>{error || status?.installer_error || status?.cli_error}</span>
                </div>
              )}
            </div>

            <footer className="codex-gate-actions">
              {(phase === "confirm" || phase === "error") && (
                <>
                  <button className="btn ghost" onClick={() => close()}>暂不安装</button>
                  <button
                    className="btn accent"
                    disabled={status?.installer_available === false}
                    onClick={() => void install()}
                    ref={primaryRef}
                  >
                    {phase === "error" ? "重试安装" : "确认并安装"}
                  </button>
                </>
              )}
              {phase === "login" && (
                <>
                  <button className="btn ghost" onClick={() => close()}>稍后登录</button>
                  <button className="btn ghost" onClick={() => void beginLogin("device")}>设备码（备用）</button>
                  <button className="btn accent" onClick={() => void beginLogin("browser")} ref={primaryRef}>使用浏览器登录</button>
                </>
              )}
              {phase === "waiting-login" && (
                <>
                  <button className="btn ghost" onClick={() => close()}>取消等待</button>
                  <button className="btn accent" onClick={() => void checkLogin()} ref={primaryRef}>重新检测</button>
                </>
              )}
              {phase === "login-timeout" && (
                <>
                  <button className="btn ghost" onClick={() => close()}>稍后处理</button>
                  <button className="btn ghost" onClick={() => void checkLogin()}>重新检测</button>
                  <button className="btn ghost" onClick={() => void beginLogin("device")}>使用设备码</button>
                  <button className="btn accent" onClick={() => void beginLogin("browser")} ref={primaryRef}>重新打开浏览器</button>
                </>
              )}
            </footer>
          </div>
        </div>,
        document.body,
      )}
    </CodexCliGateContext.Provider>
  );
}

function gateTitle(phase: GatePhase): string {
  if (phase === "installing") return "正在安装 Codex CLI";
  if (phase === "login" || phase === "waiting-login") return "登录 Codex";
  if (phase === "login-timeout") return "尚未检测到登录完成";
  if (phase === "resuming") return "准备完成";
  if (phase === "error") return "安装没有完成";
  return "需要安装 Codex CLI";
}

function gateDescription(phase: GatePhase, feature: string): string {
  if (phase === "installing") return `安装完成后，R-Code 会重新检测并继续“${feature}”。`;
  if (phase === "login") return `“${feature}”还需要登录。R-Code 只打开 Codex 官方登录流程，不接触你的凭据。`;
  if (phase === "waiting-login") return "请在刚打开的系统终端和浏览器中完成授权。";
  if (phase === "login-timeout") return `R-Code 已停止自动检测，避免长期占用资源；本次等待上限为 ${CODEX_LOGIN_WAIT_MINUTES} 分钟。`;
  if (phase === "resuming") return "安装和登录状态已通过检测。";
  if (phase === "error") return "没有修改 R-Code 项目文件。你可以重试，或复制命令到系统终端执行。";
  return `“${feature}”由本机 Codex CLI 提供。确认后，R-Code 将执行以下固定命令：`;
}
