import { useEffect, useState } from "react";
import { executionEnvProbe, settingsSet } from "../../lib/ipc";
import { pushToast } from "../../store/toast";
import { IconRefresh } from "../icons";

/** R-OPS-01 执行环境探测结果（后端 cmd_execution_env_probe）。 */
export type ExecutionEnvProbe = {
  dialect: string;
  program: string;
  git_bash_detected: boolean;
  /** 已保存的 execution.bash_shell_path（null=自动探测；""=强制回落）。 */
  configured_override?: string | null;
};

const DIALECT_LABELS: Record<string, string> = {
  "git-bash": "Git Bash（第一方言）",
  pwsh: "PowerShell 7（回落档）",
  powershell: "Windows PowerShell 5.1（回落档）",
  cmd: "cmd.exe（回落档）",
  "posix-sh": "/bin/sh",
};

type ProbeState =
  | { status: "loading" }
  | { status: "ready"; probe: ExecutionEnvProbe }
  | { status: "error"; message: string };

export function ExecutionEnvCard() {
  const [state, setState] = useState<ProbeState>({ status: "loading" });
  const [bashPath, setBashPath] = useState("");
  const [saving, setSaving] = useState(false);

  const refresh = async () => {
    setState({ status: "loading" });
    try {
      const probe = await executionEnvProbe();
      setState({ status: "ready", probe });
      // 回显已保存的覆盖值（null=自动探测 → 空输入框；空串=强制回落 → 如实显示）。
      setBashPath(probe.configured_override ?? "");
    } catch (cause) {
      setState({ status: "error", message: String(cause) });
    }
  };

  useEffect(() => {
    void refresh();
  }, []);

  const savePath = async () => {
    setSaving(true);
    try {
      await settingsSet("execution.bash_shell_path", bashPath);
      pushToast({ kind: "success", title: "已保存执行环境设置" });
      await refresh();
    } catch (cause) {
      pushToast({ kind: "error", title: "保存执行环境设置失败", body: String(cause) });
    } finally {
      setSaving(false);
    }
  };

  const dialectLabel =
    state.status === "ready" ? (DIALECT_LABELS[state.probe.dialect] ?? state.probe.dialect) : null;

  return (
    <section className="settings-block" id="execution-env-block" data-testid="execution-env-card">
      <div className="section-heading">
        <h3>执行环境（Windows Shell）</h3>
        <button type="button" className="btn sm ghost" onClick={() => void refresh()} title="重新探测">
          <IconRefresh width={14} height={14} />
        </button>
      </div>
      <p className="hint">
        bash 工具在 Windows 优先经 Git Bash 执行；未检出时回落 PowerShell。留空=自动探测；
        填写绝对路径=覆盖探测（路径必须存在，否则命令报错不静默回落）；清空保存为空串=强制回落。
      </p>
      {state.status === "loading" && <p className="dim">正在探测当前 shell 解析档…</p>}
      {state.status === "ready" && (
        <div data-testid="execution-env-probe" data-dialect={state.probe.dialect}>
          <span>
            当前方言档：<strong>{dialectLabel}</strong>
          </span>
          {state.probe.git_bash_detected ? (
            <p className="hint" data-testid="execution-env-detected">
              Git Bash 已检出：{state.probe.program}
            </p>
          ) : (
            <div className="errbar" role="alert" data-testid="execution-env-warning">
              未检出 Git Bash——bash 工具将回落 PowerShell（grep/sed 等 Unix 工具不可用）。
              安装 Git for Windows，或在下方填写 bash.exe 的绝对路径。
            </div>
          )}
        </div>
      )}
      {state.status === "error" && (
        <div className="errbar" role="alert">
          探测失败：{state.message}
        </div>
      )}
      <div className="field">
        <label htmlFor="execution-bash-path">bash 路径覆盖（绝对路径或空）</label>
        <input
          id="execution-bash-path"
          className="input"
          data-testid="execution-bash-path-input"
          value={bashPath}
          placeholder="C:\\Program Files\\Git\\bin\\bash.exe"
          onChange={(event) => setBashPath(event.target.value)}
        />
        <button
          type="button"
          className="btn sm accent"
          onClick={() => void savePath()}
          disabled={saving}
        >
          {saving ? "保存中…" : "保存"}
        </button>
      </div>
    </section>
  );
}
