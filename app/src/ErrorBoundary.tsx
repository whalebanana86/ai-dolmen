import { Component, ReactNode } from "react";

interface Props {
  /** Shown instead of the children when they fail to render. */
  fallback: (error: Error) => ReactNode;
  children: ReactNode;
}

/** Keeps a failing panel from blanking the whole window. */
export class ErrorBoundary extends Component<Props, { error: Error | null }> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error) {
    console.error("a panel failed to render", error);
  }

  render() {
    return this.state.error ? this.props.fallback(this.state.error) : this.props.children;
  }
}
