import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Maximize2, Minus, Plus, RotateCcw, X } from "lucide-react";
import type { CustomRendererProps } from "streamdown";
import { Button } from "@/ui/button";
import { cn } from "@/ui/utils";
import * as echarts from "echarts/core";
import { GeoComponent, TooltipComponent } from "echarts/components";
import { LinesChart, EffectScatterChart, ScatterChart } from "echarts/charts";
import { CanvasRenderer } from "echarts/renderers";
import chinaGeo from "@/assets/geo/china.json";
import { mapPoints, parseSpec, routeName, type MapSpec } from "./flightMapData";
import { layoutMapLabels, pointData, routeData } from "./flightMapLabels";

// 航线地图：由正文里的 ```flightmap 围栏代码块触发（streamdown 自定义 language
// renderer，见 AiMarkdown 的 plugins.renderers 注册）。中国省级边界底图 +
// 机场标记 + 航线；支持缩放、平移、展开大图和点击地点查看信息。
//
// 底图数据本地打包（@/assets/geo/china.json，阿里 DataV 省级边界），**离线、不请求
// 网络、不需要任何地图瓦片服务**。ECharts 按需引入，只带 geo/lines/scatter。
//
// 数据契约（JSON，写在 ```flightmap 代码块里）：
//   {
//     "title": "北京出发随心飞可达城市",            // 可选
//     "origin": { "name": "北京", "code": "PEK", "lat": 40.08, "lon": 116.58 },
//     "points": [
//       { "name": "三亚", "code": "SYX", "lat": 18.25, "lon": 109.51, "label": "HU7079 08:00" }
//     ]
//   }
// 行程使用 path 按 code/name 顺序连接；closed: true 自动补上末点到首点。
// 逐段备注使用 routes: [{from, to, label?}]，不能从地点备注猜测真实航段。

// Importing extensions does not install them in echarts/core. Geo must be
// installed before registerMap, and Canvas before creating a chart.
echarts.use([GeoComponent, TooltipComponent, LinesChart, EffectScatterChart, ScatterChart, CanvasRenderer]);
echarts.registerMap("china", chinaGeo as never);

type ThemeColors = {
  ink: string;
  muted: string;
  land: string;
  landEmphasis: string;
  border: string;
  accent: string;
  arc: string;
  dark: boolean;
};

function readTheme(): ThemeColors {
  const style = getComputedStyle(document.documentElement);
  const v = (name: string, fallback: string) => style.getPropertyValue(name).trim() || fallback;
  const dark = document.documentElement.dataset.theme === "dark";
  return {
    ink: v("--color-ink", dark ? "#f1f1ed" : "#171715"),
    muted: v("--color-muted", dark ? "#aaa9a2" : "#676763"),
    land: dark ? "#20201e" : "#efefec",
    landEmphasis: dark ? "#2c2c29" : "#e6e6e1",
    border: dark ? "rgba(241,241,237,0.16)" : "rgba(23,23,21,0.14)",
    accent: v("--color-accent", dark ? "#cbd65c" : "#dbe66a"),
    arc: dark ? "rgba(203,214,92,0.55)" : "rgba(120,132,20,0.55)",
    dark,
  };
}

function buildOption(spec: MapSpec, colors: ThemeColors): echarts.EChartsCoreOption {
  const origin = spec.origin;
  const all = mapPoints(spec);
  const lons = all.map((p) => p.lon);
  const lats = all.map((p) => p.lat);
  // Include curve control points in the initial extent so separated return
  // lanes are not clipped. GeoJSON's raw projection uses aspectScale 0.75.
  for (const route of spec.routes) {
    const from = all[route.from];
    const to = all[route.to];
    lons.push((from.lon + to.lon) / 2 + (from.lat - to.lat) * route.curveness / 0.75);
    lats.push((from.lat + to.lat) / 2 + (to.lon - from.lon) * route.curveness * 0.75);
  }
  const spanLon = Math.max(...lons) - Math.min(...lons);
  const spanLat = Math.max(...lats) - Math.min(...lats);
  const center: [number, number] = [
    (Math.max(...lons) + Math.min(...lons)) / 2,
    (Math.max(...lats) + Math.min(...lats)) / 2,
  ];
  // Fit the route extent rather than guessing a zoom from the China base map.
  const padLon = Math.max(spanLon * 0.2, 1);
  const padLat = Math.max(spanLat * 0.2, 1);

  const markers = pointData(spec);

  const markerLabel = (params: { name?: string; data?: { airportCode?: string; detail?: string } }) => {
    const name = `${params.name ?? ""}${params.data?.airportCode ? ` (${params.data.airportCode})` : ""}`;
    return params.data?.detail ? `${name}\n${params.data.detail}` : name;
  };

  return {
    backgroundColor: "transparent",
    animation: false,
    tooltip: {
      trigger: "item",
      renderMode: "richText",
      confine: true,
      backgroundColor: colors.dark ? "#222220" : "#fdfdfc",
      borderColor: colors.border,
      borderWidth: 1,
      padding: [6, 10],
      textStyle: { color: colors.ink, fontSize: 12 },
      formatter: markerLabel,
    },
    geo: {
      map: "china",
      aspectScale: 0.75,
      // With all four edges set, ECharts 6 fills the box independently on each
      // axis unless aspect preservation is explicit. Keep the north-up map
      // proportional in both the transcript and the expanded dialog.
      preserveAspect: "contain",
      preserveAspectAlign: "center",
      preserveAspectVerticalAlign: "middle",
      roam: true,
      scaleLimit: { min: 0.5, max: 40 },
      zoom: 1,
      center,
      boundingCoords: [
        [Math.min(...lons) - padLon, Math.min(...lats) - padLat],
        [Math.max(...lons) + padLon, Math.max(...lats) + padLat],
      ],
      left: 28,
      right: 100,
      top: 32,
      bottom: 32,
      itemStyle: {
        areaColor: colors.land,
        borderColor: colors.border,
        borderWidth: 0.6,
      },
      emphasis: {
        disabled: true,
        itemStyle: { areaColor: colors.landEmphasis },
      },
      label: { show: false },
    },
    series: [
      {
        id: "routes",
        type: "lines",
        coordinateSystem: "geo",
        zlevel: 2,
        polyline: false,
        symbol: ["none", "arrow"],
        symbolSize: [0, 10],
        emphasis: { lineStyle: { width: 3, opacity: 1 } },
        label: {
          show: false,
          position: "insideMiddle",
          color: colors.ink,
          backgroundColor: colors.land,
          padding: [3, 5],
          borderRadius: 4,
          fontSize: 11,
        },
        effect: {
          show: true,
          period: 5,
          trailLength: 0.22,
          symbol: "arrow",
          symbolSize: 5,
          color: colors.arc,
        },
        lineStyle: { color: colors.arc, width: 1.6, opacity: 0.9 },
        data: routeData(spec),
      },
      {
        id: "destinations",
        type: "scatter",
        coordinateSystem: "geo",
        zlevel: 3,
        symbol: "circle",
        symbolSize: 6,
        emphasis: { scale: 1.8 },
        itemStyle: { color: colors.ink, borderColor: colors.land, borderWidth: 1.5 },
        label: {
          show: false,
          position: "right",
          distance: 12,
          color: colors.ink,
          fontSize: 12,
          lineHeight: 17,
          backgroundColor: colors.land,
          padding: [3, 5],
          borderRadius: 3,
          formatter: markerLabel,
        },
        data: origin ? markers.slice(1) : markers,
      },
      {
        id: "origin",
        type: "effectScatter",
        coordinateSystem: "geo",
        zlevel: 4,
        symbolSize: 9,
        showEffectOn: "render",
        rippleEffect: { brushType: "stroke", scale: 3.2 },
        itemStyle: { color: colors.accent, borderColor: colors.ink, borderWidth: 1.5 },
        label: {
          show: false,
          position: "right",
          distance: 12,
          color: colors.ink,
          fontSize: 12,
          fontWeight: "bold",
          lineHeight: 17,
          backgroundColor: colors.land,
          padding: [3, 5],
          borderRadius: 3,
          formatter: markerLabel,
        },
        data: origin ? markers.slice(0, 1) : [],
      },
    ],
  };
}

function FlightMapView({ spec, expanded, onExpand, onClose }: {
  spec: MapSpec;
  expanded?: boolean;
  onExpand?: () => void;
  onClose?: () => void;
}) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const chartRef = useRef<echarts.ECharts | null>(null);
  const failRef = useRef<(error: unknown) => void>(() => {});
  const relayoutRef = useRef<() => void>(() => {});
  const [failedSpec, setFailedSpec] = useState<MapSpec | null>(null);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [selectedRoute, setSelectedRoute] = useState<number | null>(null);
  const selectionRef = useRef({ selectedIndex, selectedRoute });
  selectionRef.current = { selectedIndex, selectedRoute };
  const all = useMemo(() => mapPoints(spec), [spec]);
  const selected = all[selectedIndex] ?? all[0];

  const operate = (action: (chart: echarts.ECharts) => void) => {
    const chart = chartRef.current;
    if (!chart || chart.isDisposed()) return;
    try {
      action(chart);
    } catch (error) {
      failRef.current(error);
    }
  };
  const zoom = (factor: number) => operate((chart) => {
    chart.dispatchAction({
      type: "geoRoam", geoIndex: 0, zoom: factor,
      originX: chart.getWidth() / 2, originY: chart.getHeight() / 2,
    });
  });
  const selectPoint = (index: number) => {
    setSelectedIndex(index);
    setSelectedRoute(null);
    operate((chart) => {
      const point = all[index];
      chart.setOption({ geo: { center: [point.lon, point.lat] } });
      chart.dispatchAction({ type: "downplay" });
      relayoutRef.current();
    });
  };
  const selectRoute = (index: number) => {
    setSelectedRoute(index);
    setSelectedIndex(spec.routes[index].to);
  };

  useEffect(() => { relayoutRef.current(); }, [selectedIndex, selectedRoute]);

  useEffect(() => {
    if (!hostRef.current) return;
    const host = hostRef.current;
    let chart: echarts.ECharts | undefined;
    let ro: ResizeObserver | undefined;
    let mo: MutationObserver | undefined;
    let layoutFrame = 0;

    const cleanup = () => {
      ro?.disconnect();
      mo?.disconnect();
      cancelAnimationFrame(layoutFrame);
      // init can register an instance before throwing; dispose it as well.
      const instance = chart ?? echarts.getInstanceByDom(host);
      try {
        if (instance && !instance.isDisposed()) instance.dispose();
      } catch (error) {
        console.error("[FlightMap] Chart disposal failed", error);
      }
      chartRef.current = null;
      relayoutRef.current = () => {};
    };
    const fail = (error: unknown) => {
      console.error("[FlightMap] Chart initialization or update failed", error);
      cleanup();
      setFailedSpec(spec);
    };
    failRef.current = fail;
    const relayout = () => {
      cancelAnimationFrame(layoutFrame);
      layoutFrame = requestAnimationFrame(() => {
        try {
          if (chart && !chart.isDisposed()) {
            const selection = selectionRef.current;
            layoutMapLabels(chart, spec, selection.selectedIndex, selection.selectedRoute);
          }
        } catch (error) { fail(error); }
      });
    };
    relayoutRef.current = relayout;

    try {
      chart = echarts.init(host, undefined, { renderer: "canvas" });
      chartRef.current = chart;
      chart.setOption(buildOption(spec, readTheme()));
      relayout();
      chart.on("georoam", relayout);
      chart.on("click", (params: { seriesIndex?: number; dataIndex?: number }) => {
        if (params.seriesIndex === 0 && typeof params.dataIndex === "number") {
          setSelectedRoute(params.dataIndex);
          setSelectedIndex(spec.routes[params.dataIndex].to);
        }
        if (params.seriesIndex === 2 && spec.origin) {
          setSelectedIndex(0);
          setSelectedRoute(null);
        }
        if (params.seriesIndex === 1 && typeof params.dataIndex === "number") {
          setSelectedIndex(params.dataIndex + (spec.origin ? 1 : 0));
          setSelectedRoute(null);
        }
      });

      ro = new ResizeObserver(() => {
        try {
          if (chart && !chart.isDisposed()) { chart.resize(); relayout(); }
        } catch (error) {
          fail(error);
        }
      });
      ro.observe(host);

      // 跟随明暗主题：data-theme 变化时重画。
      mo = new MutationObserver(() => {
        try {
          if (chart && !chart.isDisposed()) {
            const currentGeo = (chart.getOption().geo as { center?: number[]; zoom?: number }[])[0];
            const option = buildOption(spec, readTheme());
            option.geo = { ...option.geo as object, center: currentGeo.center, zoom: currentGeo.zoom };
            chart.setOption(option, true);
            relayout();
          }
        } catch (error) {
          fail(error);
        }
      });
      mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    } catch (error) {
      fail(error);
    }

    return cleanup;
  }, [spec]);

  if (failedSpec === spec) {
    return (
      <div className="my-2 rounded-lg border border-line bg-sidebar px-3 py-2 text-body-sm text-muted" role="status">
        航线地图渲染失败，其他聊天内容仍可查看。
        {onClose && <Button variant="ghost" size="sm" onClick={onClose}>关闭大图</Button>}
      </div>
    );
  }

  return (
    <div className={cn("flex min-h-0 flex-col", expanded && "h-full")}>
      <div className="flex flex-wrap items-center justify-between gap-2 border-b border-line px-3 py-2">
        <span className="text-body-sm font-medium text-ink">{spec.title ?? "航线地图"}</span>
        <div className="flex items-center gap-1" role="group" aria-label="地图操作">
          <Button variant="ghost" size="icon-sm" title="缩小" aria-label="缩小地图" onClick={() => zoom(1 / 1.4)}><Minus /></Button>
          <Button variant="ghost" size="icon-sm" title="放大" aria-label="放大地图" onClick={() => zoom(1.4)}><Plus /></Button>
          <Button variant="ghost" size="icon-sm" title="显示全部航线" aria-label="复位地图，显示全部航线" onClick={() => {
            setSelectedRoute(null);
            operate((chart) => { chart.setOption(buildOption(spec, readTheme()), true); relayoutRef.current(); });
          }}><RotateCcw /></Button>
          {onExpand && <Button variant="ghost" size="sm" onClick={onExpand}><Maximize2 />展开大图</Button>}
          {onClose && <Button variant="ghost" size="icon-sm" autoFocus aria-label="关闭地图大图" onClick={onClose}><X /></Button>}
        </div>
      </div>
      <p className="px-3 pt-2 text-xs text-muted">滚轮缩放 · 拖动平移 · 点击机场或航线查看信息 · 放大显示更多标注</p>
      <div ref={hostRef} className={cn("w-full touch-none overscroll-contain", expanded ? "min-h-0 flex-1" : "h-[420px]")} role="group" aria-label={`${spec.title ?? "航线地图"}，可缩放和平移`} />
      <div className="shrink-0 space-y-2 border-t border-line px-3 py-3 text-body-sm">
        <div className="flex flex-wrap gap-3 text-xs text-muted">
          {spec.origin && <span className="inline-flex items-center gap-1.5"><span className="size-2 rounded-full border border-ink bg-accent" />出发地</span>}
          <span className="inline-flex items-center gap-1.5"><span className="size-2 rounded-full bg-ink" />{spec.origin ? "目的地" : "地点"} · {spec.points.length} 个</span>
          {spec.routes.length > 0 && <span>→ 飞行方向 · {spec.routes.length} 段{spec.closed ? " · 首尾闭环" : ""}</span>}
        </div>
        <div className="max-h-24 overflow-y-auto">
          <div className="flex flex-wrap gap-1.5" role="group" aria-label="地图地点">
            {all.map((point, index) => (
              <Button key={index} variant={selected === point ? "secondary" : "ghost"} size="sm" className="min-h-8 px-2" aria-pressed={selected === point} onClick={() => selectPoint(index)}>
                {spec.origin && index === 0 ? "出发 · " : ""}{point.name}{point.code ? ` (${point.code})` : ""}
              </Button>
            ))}
          </div>
        </div>
        {spec.routes.length > 0 && <div className="flex max-h-24 flex-wrap gap-1 overflow-y-auto" role="group" aria-label="航段顺序">
          {spec.routes.map((route, index) => (
            <Button key={index} variant={selectedRoute === index ? "secondary" : "ghost"} size="sm" className="min-h-8 px-2" aria-pressed={selectedRoute === index} onClick={() => selectRoute(index)}>{routeName(spec, route, index)}</Button>
          ))}
        </div>}
        <div className="max-h-28 overflow-y-auto rounded-control bg-inset px-3 py-2 text-ink" aria-live="polite">
          {selectedRoute !== null ? <>
            <span className="font-medium">{routeName(spec, spec.routes[selectedRoute], selectedRoute)}</span>
            {spec.routes[selectedRoute].label && <p className="mt-1 whitespace-pre-wrap break-words">{spec.routes[selectedRoute].label}</p>}
          </> : <>
            <span className="font-medium">{selected.name}{selected.code ? ` · ${selected.code}` : ""}</span>
            {selected.label && <p className="mt-1 whitespace-pre-wrap break-words">{selected.label}</p>}
            <p className="mt-1 text-xs text-muted">经度 {selected.lon.toFixed(4)} · 纬度 {selected.lat.toFixed(4)}</p>
          </>}
        </div>
      </div>
    </div>
  );
}

export function FlightMap({ code, isIncomplete }: CustomRendererProps) {
  const spec = useMemo(() => parseSpec(code), [code]);
  const [expanded, setExpanded] = useState(false);
  const dialogRef = useRef<HTMLDialogElement | null>(null);

  useEffect(() => {
    if (!expanded || !dialogRef.current) return;
    const dialog = dialogRef.current;
    const previousFocus = document.activeElement;
    dialog.showModal();
    return () => {
      dialog.close();
      if (previousFocus instanceof HTMLElement && previousFocus.isConnected) previousFocus.focus();
    };
  }, [expanded]);

  if (!spec) {
    if (isIncomplete) {
      return (
        <div className="my-2 rounded-lg border border-line bg-sidebar px-3 py-2 text-body-sm text-muted">
          正在生成航线地图…
        </div>
      );
    }
    return (
      <div className="my-2 rounded-lg border border-line bg-sidebar px-3 py-2 text-body-sm text-muted">
        航线地图数据无法解析：请检查地点经纬度，以及 path/routes 引用的机场代码或名称。
      </div>
    );
  }

  return (
    <>
      <figure className="my-3 overflow-hidden rounded-xl border border-line bg-surface">
        <FlightMapView spec={spec} onExpand={() => setExpanded(true)} />
      </figure>
      {expanded && createPortal(
        <dialog ref={dialogRef} aria-label={`${spec.title ?? "航线地图"}大图`} className="fixed inset-0 m-auto h-[calc(100dvh-2rem)] max-h-none w-[calc(100vw-2rem)] max-w-none overflow-hidden rounded-xl border border-line bg-surface p-0 text-ink shadow-xl backdrop:bg-black/50" onCancel={() => setExpanded(false)} onClick={(event) => { if (event.target === event.currentTarget) setExpanded(false); }}>
          <FlightMapView spec={spec} expanded onClose={() => setExpanded(false)} />
        </dialog>,
        document.body,
      )}
    </>
  );
}
