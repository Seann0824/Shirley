# Task F: 前端 bridge.ts SSE + 第四态 + server 模式路径输入框（只改 web/src 下的 TS/TSX）

## 背景
Shirley 的 desktop 前端（React+TS）在 `src/interface/desktop/web/`。同一份前端有两个运行形态：
- Tauri 桌面：`isTauri` 为真 → `createTauriBridge()`（invoke/listen）
- 浏览器打开云端 server：非 Tauri 生产 → `createHttpBridge()`（现为 POST 命令 + 单条 WebSocket `/ws`）

本轮目标（**只做前端**，Rust 侧由主线程另行实现，接口契约见下）：
1. HTTP 事件面从 **WebSocket 改成 SSE**（`GET /api/events`，`text/event-stream`）。
2. 加**第四态**：Tauri 内但配置了远程 server URL → 用 `createHttpBridge(serverUrl)` 连云端。
3. 加**能力探测**：云端模式没有原生文件夹选择器，`addProject` 改为「弹路径输入框 → 调 `agent_add_project { path, name? }`」。

## 严格边界
- **只改 `src/interface/desktop/web/src/` 下的 `.ts` / `.tsx`**（主要 `lib/bridge.ts`，其次 `App.tsx` 与相关组件）。不要动任何 Rust 文件、不要动 docs。
- 保持 `AgentBridge` 类型对外形态尽量兼容（新增字段/方法是允许的，删除/改签名不允许）。
- `npm run build`（`cd src/interface/desktop/web && npm run build`）必须通过（tsc + vite）。若本机没装依赖，可只做 `npx tsc --noEmit` 级别的类型校验，并在报告里说明。

## 后端契约（Rust 侧已定，按此实现，勿改）
- 命令面不变：`POST /api/cmd/{name}`，body JSON，返回 JSON（与原 `httpCall` 一致）。
- **事件面改为 SSE**：`GET /api/events`，`Content-Type: text/event-stream`，长连接，服务端逐条下发：
  ```
  event: agent_event
  data: {"type":"content_delta","project":"...","session":"...",...}\n\n
  ```
  即：每个 SSE event 的 `event` 字段恒为 `agent_event`，`data` 字段是 `AgentEventWire` 的 JSON（结构与现有 `wire.ts` 的 `AgentEventWire` 完全一致，含 `project`/`session`）。
- `agent_add_project` 命令参数：`{ path: string, name?: string | null }`，返回 `ProjectEntry`（成功）或 `null`（失败/取消）。云端模式下由前端传路径。

## 实现要求（bridge.ts）
### 1. 能力描述
新增并导出：
```ts
export type BridgeCapabilities = {
  /** 是否有原生文件夹选择器（Tauri 桌面为 true；云端 HTTP 为 false，改用手输路径）。 */
  nativeFolderPicker: boolean;
};
```
在 `AgentBridge` 里加方法：
```ts
capabilities: () => BridgeCapabilities;
```
三个 bridge 实现都补：
- `createTauriBridge`：`nativeFolderPicker: true`
- `createMockBridge`：`nativeFolderPicker: true`（mock 行为不变）
- `createHttpBridge`：`nativeFolderPicker: false`

### 2. createHttpBridge 改造
- 签名改为 `function createHttpBridge(baseUrl: string = ""): AgentBridge`。所有 `httpCall` 的前缀用 `baseUrl`（`fetch(\`${baseUrl}/api/cmd/${name}\`, ...)`）。默认空串 = 同源（浏览器直接打开 server 时）。
- **删掉 WebSocket 逻辑**，改用 `EventSource`：
  ```ts
  const es = new EventSource(`${baseUrl}/api/events`);
  es.addEventListener("agent_event", (ev) => {
    let wire: AgentEventWire; try { wire = JSON.parse((ev as MessageEvent).data); } catch { return; }
    if (wire.project == null || wire.session == null) return;
    handlers.get(routeKey(wire.project, wire.session))?.(wire);
  });
  ```
  `EventSource` 自带断线重连，**不要再手写 setTimeout 重连**。`unsubscribe` 只删 handler，不 close（全局一条流）。如担心资源，可在 `disposed` 时 `es.close()`，但当前无全局 dispose 入口，可不处理。
- `addProject` 改为：**不再无参调用**。因为签名 `addProject: () => Promise<ProjectEntry | null>` 不能改（Tauri 侧无参），云端能力通过新方法承载：
  新增 `AgentBridge` 方法
  ```ts
  /** 仅云端模式：用服务端路径创建/打开项目。Tauri 模式不实现（调用应抛错）。 */
  addProjectByPath: (path: string, name?: string | null) => Promise<ProjectEntry | null>;
  ```
  - `createHttpBridge`：`addProjectByPath = (path, name) => httpCall("agent_add_project", { path, name })`；`addProject` 在 http 模式可返回 `null` 或抛「use addProjectByPath」——**推荐**：http 模式 `addProject` 直接 `return null`（UI 会走 addProjectByPath 分支）。
  - `createTauriBridge` / `createMockBridge`：`addProjectByPath` 抛错（`throw new Error("addProjectByPath is only available in server mode")`）即可。

### 3. 第四态 agentBridge()
- 远程 server URL 来源（按优先级）：
  1. `localStorage.getItem("shirley.serverUrl")`
  2. `(window as any).__SHIRLEY_SERVER__`
- 逻辑：
  ```ts
  const remoteUrl = (typeof localStorage !== "undefined" && localStorage.getItem("shirley.serverUrl")) ||
                    (window as any).__SHIRLEY_SERVER__ || "";
  cached ??= isTauri
    ? (remoteUrl ? Promise.resolve(createHttpBridge(remoteUrl)) : createTauriBridge())
    : dev ? Promise.resolve(createMockBridge())
          : Promise.resolve(createHttpBridge());  // 同源
  ```
- 保持原有 mock / 生产 http / Tauri 三态不变，只插一态。

### 4. App.tsx 的 addProject 分叉
现有 `App.tsx:584` 的 `addProject` 回调调 `bridge.addProject()`。改为：
```ts
const caps = bridge.capabilities();  // bridge 需先在作用域内拿到
if (caps.nativeFolderPicker) {
  const entry = await bridge.addProject();
  ...
} else {
  const path = window.prompt("输入服务器上的项目目录绝对路径：");
  if (!path) return;
  const entry = await bridge.addProjectByPath(path.trim());
  ...
}
```
- 用 `window.prompt` 做最小实现即可（不要引入新依赖、不要新建复杂模态）。如担心风格，可复用现有对话框组件，但**以能跑通为先**。
- 注意 `bridge` 在 App.tsx 里如何取得（看现有代码，通常是 `agentBridge()` 的 promise）。保持最小改动。
- `capabilities()` 是同步方法，但 bridge 本身是异步获取的；在 `addProject` 回调内部（已 async）先 `const b = await agentBridge()` 再 `b.capabilities()`。

## 验收
- `npx tsc --noEmit`（在 web 目录）或 `npm run build` 通过。
- 逐项自查：WebSocket 相关代码已全部移除；`agent_event` 监听已接上；第四态逻辑就位；`addProjectByPath` 三实现齐全；App.tsx 分叉到位。

## 交付报告
回复：改了哪些文件/函数、EventSource 接入点、第四态判断逻辑、addProjectByPath 三实现、App.tsx 分叉方式、类型检查/build 结果（贴关键输出）。若因缺 node_modules 无法 build，说明并给出 `npx tsc --noEmit` 结果。
