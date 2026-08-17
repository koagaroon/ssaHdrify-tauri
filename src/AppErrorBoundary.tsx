import { Component, type ErrorInfo, type ReactNode } from "react";
import "./shell.css";

interface Props {
  children: ReactNode;
}

interface State {
  hasError: boolean;
}

export default class AppErrorBoundary extends Component<Props, State> {
  state: State = { hasError: false };

  static getDerivedStateFromError(): State {
    return { hasError: true };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error("[ssaHdrify] React render failed", error, info.componentStack);
  }

  render(): ReactNode {
    if (!this.state.hasError) return this.props.children;

    return (
      <main className="app-error-boundary" role="alert" aria-labelledby="app-error-title">
        <section className="app-error-panel">
          <h1 id="app-error-title">SSA HDRify ran into a display error</h1>
          <p>Reload the app to try again. If the error returns, close and reopen SSA HDRify.</p>
          <p lang="zh-CN">
            SSA HDRify 界面发生错误。请重新加载应用；若错误再次出现，请关闭后重新打开。
          </p>
          <button type="button" onClick={() => window.location.reload()}>
            Reload app / 重新加载应用
          </button>
        </section>
      </main>
    );
  }
}
