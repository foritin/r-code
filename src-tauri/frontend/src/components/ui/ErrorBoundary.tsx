/**
 * 全应用错误边界。
 *
 * 在这之前应用一个都没有：任何组件在渲染期抛错（比如工具卡片碰上深嵌套 JSON、
 * markdown 解析器吃到病态输入），React 18 会卸载整棵树 —— 用户看到的是纯白窗口，
 * 只能重启应用，且会话里没落盘的内容一并丢失。
 *
 * 这里不试图"恢复"：渲染期错误往往意味着状态已不可信。给的是一条明确的出路
 * （重试渲染 / 重载窗口）和可复制的诊断信息，比白屏诚实。
 */
import { Component, type ErrorInfo, type ReactNode } from "react";

interface Props {
  children: ReactNode;
}

interface State {
  error: Error | null;
  stack: string | null;
}

export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null, stack: null };

  static getDerivedStateFromError(error: Error): Partial<State> {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    // 控制台是唯一还可靠的通道；此时 store 和 IPC 都可能处于半损状态。
    console.error("[r-code] 渲染期未捕获错误", error, info.componentStack);
    this.setState({ stack: info.componentStack ?? null });
  }

  private reset = (): void => {
    this.setState({ error: null, stack: null });
  };

  private reload = (): void => {
    window.location.reload();
  };

  render(): ReactNode {
    const { error, stack } = this.state;
    if (!error) return this.props.children;

    const detail = `${error.name}: ${error.message}\n${error.stack ?? ""}\n${stack ?? ""}`;
    return (
      <div className="crash" role="alert">
        <div className="crash-box">
          <h1>界面崩了</h1>
          <p>
            这一屏渲染时抛了异常。会话数据在主进程里，没有丢；重载窗口通常就能回到原来的地方。
          </p>
          <pre className="crash-detail">{detail.trim()}</pre>
          <div className="crash-actions">
            <button type="button" className="btn" onClick={this.reset}>
              重试渲染
            </button>
            <button type="button" className="btn accent" onClick={this.reload}>
              重载窗口
            </button>
          </div>
        </div>
      </div>
    );
  }
}
