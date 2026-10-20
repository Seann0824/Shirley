export type MapPoint = { name: string; code?: string; label?: string; lat: number; lon: number };
export type MapRoute = { from: number; to: number; label?: string; curveness: number };
export type MapSpec = { title?: string; origin?: MapPoint; points: MapPoint[]; routes: MapRoute[]; closed: boolean };

function toPoint(raw: unknown): MapPoint | null {
  if (!raw || typeof raw !== "object") return null;
  const obj = raw as Record<string, unknown>;
  const number = (value: unknown) => typeof value === "number" || (typeof value === "string" && value.trim()) ? Number(value) : NaN;
  const lat = number(obj.lat);
  const lon = number(obj.lon);
  if (!Number.isFinite(lat) || !Number.isFinite(lon) || Math.abs(lat) > 90 || Math.abs(lon) > 180) return null;
  const text = (value: unknown) => typeof value === "string" ? value.trim() : "";
  const name = text(obj.name) || text(obj.code);
  return name ? { name, lat, lon, code: text(obj.code) || undefined, label: text(obj.label) || undefined } : null;
}

export function mapPoints(spec: Pick<MapSpec, "origin" | "points">): MapPoint[] {
  return spec.origin ? [spec.origin, ...spec.points] : spec.points;
}

export function parseSpec(code: string): MapSpec | null {
  let raw: unknown;
  try { raw = JSON.parse(code); } catch { return null; }
  if (!raw || typeof raw !== "object") return null;
  const obj = raw as Record<string, unknown>;
  const points = (Array.isArray(obj.points) ? obj.points : []).map(toPoint).filter((p): p is MapPoint => p !== null);
  const origin = toPoint(obj.origin) ?? undefined;
  const all = mapPoints({ origin, points });
  if (!all.length) return null;
  const resolve = (ref: unknown) => {
    if (typeof ref === "number" && Number.isInteger(ref) && ref >= 0 && ref < all.length) return ref;
    if (typeof ref !== "string") return -1;
    const matches = all.map((p, i) => p.code === ref || p.name === ref ? i : -1).filter((i) => i >= 0);
    return matches.length === 1 ? matches[0] : -1;
  };
  const routes: MapRoute[] = [];
  const add = (from: number, to: number, label?: unknown) => {
    if (from < 0 || to < 0) return false;
    // Repeated visits belong in the path, not in duplicate markers/zero-length legs.
    if (all[from].lat === all[to].lat && all[from].lon === all[to].lon) return true;
    routes.push({ from, to, label: typeof label === "string" ? label.trim() || undefined : undefined, curveness: 0.16 });
    return true;
  };
  const closed = obj.closed === true || obj.mode === "loop";
  if (obj.routes !== undefined) {
    if (!Array.isArray(obj.routes) || obj.path !== undefined) return null;
    for (const rawRoute of obj.routes) {
      if (!rawRoute || typeof rawRoute !== "object") return null;
      const route = rawRoute as Record<string, unknown>;
      if (!add(resolve(route.from), resolve(route.to), route.label)) return null;
    }
    // Explicit routes define their own topology; don't invent an extra leg.
    if (closed && routes.length && routes[routes.length - 1].to !== routes[0].from) return null;
  } else if (obj.path !== undefined || obj.mode === "itinerary" || closed) {
    if (obj.path !== undefined && !Array.isArray(obj.path)) return null;
    const path = Array.isArray(obj.path) ? obj.path.map(resolve) : all.map((_, i) => i);
    if (!path.length || path.some((i) => i < 0)) return null;
    for (let i = 1; i < path.length; i++) add(path[i - 1], path[i]);
    if (closed && path.length > 1 && path.at(-1) !== path[0]) add(path[path.length - 1], path[0]);
  } else if (origin && obj.mode !== "points") {
    points.forEach((_, i) => add(0, i + 1));
  }

  // Assign separate physical lanes to repeated/return legs. Reversing endpoints
  // also reverses the curve's normal, so adjust the sign for that direction.
  const coordinateKey = (p: MapPoint) => `${p.lon},${p.lat}`;
  const groups = new Map<string, MapRoute[]>();
  for (const route of routes) {
    const pair = [coordinateKey(all[route.from]), coordinateKey(all[route.to])].sort().join("|");
    groups.set(pair, [...(groups.get(pair) ?? []), route]);
  }
  for (const group of groups.values()) {
    group.forEach((route, lane) => {
      const direction = coordinateKey(all[route.from]) < coordinateKey(all[route.to]) ? 1 : -1;
      const spacing = Math.min(0.24, 1.2 / Math.max(1, group.length - 1));
      const offset = group.length === 1 ? 0.16 : (lane - (group.length - 1) / 2) * spacing;
      route.curveness = offset * direction;
    });
  }
  const isCircuit = routes.length > 0 && routes.at(-1)!.to === routes[0].from && routes.every((route, i) => i === 0 || routes[i - 1].to === route.from);
  return { title: typeof obj.title === "string" ? obj.title.trim() || undefined : undefined, origin, points, routes, closed: isCircuit };
}

export function routeName(spec: MapSpec, route: MapRoute, index: number): string {
  const all = mapPoints(spec);
  return `${index + 1}. ${all[route.from].name} → ${all[route.to].name}`;
}
