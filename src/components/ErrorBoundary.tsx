import React from "react";

interface Props {
  children: React.ReactNode;
  /** Optional label so a scoped boundary can name the area that failed. */
  label?: string;
}

interface State {
  error: Error | null;
}

/**
 * Catches render/lifecycle errors so one broken component shows a recoverable
 * panel instead of white-screening the whole app.
 */
export class ErrorBoundary extends React.Component<Props, State> {
  override state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  override componentDidCatch(error: Error, info: React.ErrorInfo) {
    console.error("[ErrorBoundary]", this.props.label ?? "app", error, info.componentStack);
  }

  private reset = () => this.setState({ error: null });

  override render() {
    const { error } = this.state;
    if (!error) return this.props.children;

    return (
      <div className="error-boundary" role="alert">
        <div className="error-boundary-card">
          <div className="error-boundary-icon">⚠</div>
          <div className="error-boundary-title">
            {this.props.label ? `${this.props.label} hit an error` : "Something broke"}
          </div>
          <div className="error-boundary-msg">{error.message || "Unexpected error"}</div>
          <div className="error-boundary-actions">
            <button className="error-boundary-btn primary" onClick={this.reset}>
              Try again
            </button>
            <button className="error-boundary-btn" onClick={() => window.location.reload()}>
              Reload app
            </button>
          </div>
        </div>
      </div>
    );
  }
}
