import { Component, lazy, Suspense, type ErrorInfo, type ReactNode } from "react";
import type { CustomRendererProps } from "streamdown";

// Keep the chart library and map data out of the application startup path.
const LazyFlightMap = lazy(() =>
  import("@/components/ai/FlightMap").then(({ FlightMap }) => ({ default: FlightMap })),
);

class FlightMapErrorBoundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("[FlightMap] Loading or rendering failed", error, info);
  }

  render() {
    if (this.state.failed) {
      return (
        <div className="my-2 rounded-lg border border-line bg-sidebar px-3 py-2 text-body-sm text-muted" role="status">
          航线地图加载失败，其他聊天内容仍可查看。
        </div>
      );
    }
    return this.props.children;
  }
}

export function FlightMapRenderer(props: CustomRendererProps) {
  return (
    <FlightMapErrorBoundary key={props.code}>
      <Suspense fallback={
        <div className="my-2 rounded-lg border border-line bg-sidebar px-3 py-2 text-body-sm text-muted" role="status">
          正在加载航线地图…
        </div>
      }>
        <LazyFlightMap {...props} />
      </Suspense>
    </FlightMapErrorBoundary>
  );
}
