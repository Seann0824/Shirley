import type { ECharts } from "echarts/core";
import { mapPoints, routeName, type MapSpec } from "./flightMapData";

export function pointData(spec: MapSpec) {
  return mapPoints(spec).map((p) => ({ name: p.name, value: [p.lon, p.lat], airportCode: p.code, detail: p.label }));
}

export function routeData(spec: MapSpec) {
  const all = mapPoints(spec);
  return spec.routes.map((route, index) => ({
    name: routeName(spec, route, index),
    detail: route.label,
    coords: [[all[route.from].lon, all[route.from].lat], [all[route.to].lon, all[route.to].lat]],
    lineStyle: { curveness: route.curveness },
  }));
}

type Rect = { x: number; y: number; width: number; height: number };
type Position = "right" | "left" | "top" | "bottom";

// Recompute in screen pixels after each roam/resize. Geographic distance alone
// cannot tell whether Chinese labels fit, or when zooming reveals enough space.
export function layoutMapLabels(chart: ECharts, spec: MapSpec, selectedIndex: number, selectedRoute: number | null) {
  const width = chart.getWidth();
  const height = chart.getHeight();
  if (!width || !height) return;
  const zoom = (chart.getOption().geo as { zoom: number }[])[0].zoom;
  const all = mapPoints(spec);
  const pixels = all.map((p) => chart.convertToPixel({ geoIndex: 0 }, [p.lon, p.lat]) as number[]);
  const occupied: Rect[] = pixels.filter((p) => p && p.every(Number.isFinite)).map(([x, y]) => ({ x: x - 7, y: y - 7, width: 14, height: 14 }));
  const context = document.createElement("canvas").getContext("2d");
  if (context) context.font = "12px -apple-system, BlinkMacSystemFont, 'PingFang SC', sans-serif";
  const fits = (box: Rect) => box.x >= 5 && box.y >= 5 && box.x + box.width <= width - 5 && box.y + box.height <= height - 5 && !occupied.some((r) =>
    box.x < r.x + r.width + 4 && box.x + box.width + 4 > r.x && box.y < r.y + r.height + 4 && box.y + box.height + 4 > r.y,
  );
  const textWidth = (text: string) => context?.measureText(text).width ?? text.length * 12;
  const compact = (text: string) => text.length > 32 ? `${text.slice(0, 31)}…` : text;
  const data = pointData(spec).map((p) => ({ ...p, label: { show: false, position: "right" as Position, formatter: "" } }));
  const priority = [...all.keys()].sort((a, b) => (a === selectedIndex ? -3 : a === 0 && spec.origin ? -2 : a) - (b === selectedIndex ? -3 : b === 0 && spec.origin ? -2 : b));
  for (const index of priority) {
    const pixel = pixels[index];
    if (!pixel || !pixel.every(Number.isFinite)) continue;
    const point = all[index];
    const name = `${point.name}${zoom >= 2 && point.code ? ` (${point.code})` : ""}`;
    const variants = [...new Set([...(zoom >= 3 && point.label ? [`${name}\n${compact(point.label)}`] : []), name, point.name])];
    // If added detail does not fit, retain the shorter city label rather than
    // making an already visible city disappear at the next zoom threshold.
    for (const text of variants) {
      const lines = text.split("\n");
      const w = Math.max(...lines.map(textWidth)) + 12;
      const h = lines.length * 17 + 8;
      const [x, y] = pixel;
      const candidates: [Position, Rect][] = [
        ["right", { x: x + 14, y: y - h / 2, width: w, height: h }],
        ["left", { x: x - 14 - w, y: y - h / 2, width: w, height: h }],
        ["top", { x: x - w / 2, y: y - 14 - h, width: w, height: h }],
        ["bottom", { x: x - w / 2, y: y + 14, width: w, height: h }],
      ];
      const placement = candidates.find(([, box]) => fits(box));
      if (placement) {
        occupied.push(placement[1]);
        data[index].label = { show: true, position: placement[0], formatter: text };
        break;
      }
    }
  }
  const routes = routeData(spec).map((item, index) => {
    const route = spec.routes[index];
    const a = pixels[route.from];
    const b = pixels[route.to];
    let show = false;
    if (a && b && zoom >= 1) {
      // Quadratic midpoint, matching ECharts' linesLayout control point.
      const x = (a[0] + b[0]) / 2 - (a[1] - b[1]) * route.curveness / 2;
      const y = (a[1] + b[1]) / 2 - (b[0] - a[0]) * route.curveness / 2;
      const size = Math.max(32, textWidth(`${index + 1}`) + 20);
      const box = { x: x - size / 2, y: y - size / 2, width: size, height: size };
      show = Math.hypot(a[0] - b[0], a[1] - b[1]) > 60 && fits(box);
      if (show) occupied.push(box);
    }
    return {
      ...item,
      lineStyle: { ...item.lineStyle, width: index === selectedRoute ? 3 : 1.6, opacity: selectedRoute === null || index === selectedRoute ? 0.9 : 0.3 },
      label: { show, formatter: `${index + 1}` },
    };
  });
  chart.setOption({ series: [
    { id: "routes", data: routes },
    { id: "destinations", data: spec.origin ? data.slice(1) : data },
    { id: "origin", data: spec.origin ? data.slice(0, 1) : [] },
  ] }, { silent: true });
}
