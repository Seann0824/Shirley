# 渲染契约：RenderPart 与可扩展的呈现口子

> 状态：**P0 已落地**（本文档即 P0 的重构计划）。P1-P3 未落地，规格已定。
> 相关文档：`docs/desktop-interface.md`（wire 层）、`docs/multimodal.md`（输入侧 `ContentPart`）、`docs/session.md`（会话持久化）。

## 1. 一句话

`Message` 同时充当**模型请求体**、**会话日志**和**界面数据源**——这三重身份在纯文本时恰好重合，一旦有图片 / 文件 / 结构化产物就必然撕裂。修法不是给 `Message` 加变体，而是**把渲染视图从模型视图里拆出来**，并在渲染视图上开一个**注册表形状**的口子。

```
模型视图  Message        （SDK，不变；只描述"发给模型的东西"）
渲染视图  RenderPart     （应用层；只描述"给人看的东西"，必须能降级成文本）
带外通道  Artifact       （字节不进 JSON，只进 id）
```

## 2. 不合理之处（现状证据）

| # | 症状 | 代码位置 | 后果 |
| --- | --- | --- | --- |
| 1 | 工具产物在**唯一不知道 payload 语义**的层被压平 | `sdk/src/runtime/agent.rs:552` `output: content.to_string()`；`desktop/wire.rs:91` `output: String` | `ToolFuture` 返回的 `serde_json::Value` 结构当场销毁，UI 只能拿文本再猜 |
| 2 | `apply_event` 对工具完成是 no-op | `interface/session/mod.rs:337-339` | 工具输出只活在**前端实时内存**里，切会话 / 重启即丢 |
| 3 | 还原对话时跳过 tool 消息 | `items_from_messages` 的 `Message::Tool { .. } => {}`（`session/mod.rs:429`） | 同上，从日志重建的视图天然缺工具结果 |
| 4 | 投影只取文本 | `content.as_text()` 只拼 `ContentPart::Text`（`sdk/src/message/mod.rs:118`） | 粘贴的图**已经持久化进 JSONL**，但还原时被静默丢弃：存了渲不出，渲得出存不下 |
| 5 | 富渲染是硬编码闭集 | `AiToolActivityDisclosure.tsx` 的 `TOOL_LABELS`、`lib/structuredReport.ts`（正则扒 XML）、`FlightMap.tsx` / `BrowserView.tsx` | 新增媒介 = 改核心组件 + 改名字映射，等于没有口子 |
| 6 | 没有资产通道 | `tauri.conf.json` `csp: null`、无 `assetProtocol`；`shell.rs:696` 无读字节命令 | 即使 payload 说"这是图"，webview 也拿不到字节；唯一出路是 `data:` URL 把 base64 灌进 IPC 与日志 |
| 7 | 非 web 界面没有降级契约 | 现状是靠 `AgentEventWire::Notice` 发一句"图片已降级" | TUI / cloud web / 日志 / 模型请求各自都要"认识新媒介"，否则就是空白 |

根因收敛成一句：**渲染契约放错了层**。放对了层，上面 7 条全部是"缺上下文"，不是"缺特判"。

## 3. 归属表：什么是封装的，什么是外置的

| | 内容 | 归属 | 本轮 |
| --- | --- | --- | --- |
| **不变量** | `RenderPart` 的形状 + `fallback_text()` 必须存在 | `interface/view.rs`（应用层，三界面共用） | ✅ |
| | 传输纪律：`output` 给模型、`parts` 给人、JSON 里永不带像素 | `desktop/wire.rs` + `types/wire.ts` | ✅ |
| | 对齐与持久化：`tool_call_id` 关联、切会话不丢 | `interface/session` | ✅ |
| | 资产 jail：路径越界 / mime / 大小 / 不接受外部指定 scheme | `domain/artifact.rs` + 传输层 | P1 |
| **变化量** | 有哪些 part kind、每种长什么样 | 工具 / skill（产 `parts` 数组） | ✅ 契约 |
| | 某个 kind / schema 用什么组件 | 前端注册表 `RenderParts.tsx` | ✅ |
| | 新媒介、新报告形态 | 加 kind + 加组件，核心不动 | P2 |

**边界判据**：外置的是"随媒介与呈现形态增长"的维度；"随信任边界变化"的必须留在核心里。`parts` 只带数据，绝不带 HTML / 脚本；`Data` 的 schema 必须是本机注册过的，远程工具自封一个 schema 名不构成渲染授权。一旦允许外部注册"渲染时执行的回调"，`Data` 就退化成 iframe，恶意工具能伪造出"请点这里授权"的假界面。

**口子的形状是注册表，不是钩子。** 这和仓库既有立场一致：`ContextProvider` 是数据注入缝、`ToolContext` 是依赖注入缝、`extra_body` 是请求体逃生口，都是"你给数据，我负责送"。渲染口子属于同一族。

## 4. 契约

```rust
// interface/view.rs —— 应用层，SDK 零改动
pub enum AssetSource {
    /// 带外资产：字节由应用提供，JSON 里只有 id（P1 落地）。
    Artifact { id: String },
    /// 外部地址（https / data:）。允许，但由传输层限长。
    Url { url: String },
}

pub enum RenderPart {
    Text  { text: String },
    Image { source: AssetSource, alt: String, mime: Option<String> },
    File  { path: String, name: String, mime: Option<String>, bytes: Option<u64> },
    Link  { url: String, title: String },
    Data  { schema: String, value: serde_json::Value },
}
```

序列化为 `#[serde(tag = "kind", rename_all = "snake_case")]`，前端按 `kind` 分发。四条纪律：

1. **every part 必须能降级**。`fallback_text()` 是强制项，TUI / cloud web / 日志 / 模型请求共用它——"不认识新媒介"永远不是渲染空白或报错的理由。
2. **只放引用，不放像素**。图 = `AssetSource`，文件 = 路径 + 元信息，绝不把字节塞进事件或 JSONL。
3. **未知 kind 静默跳过**。前端 / 后端解析 `parts` 时遇到不认识的 kind 丢弃而不是报错，保证旧端能读新数据（前向兼容）。
4. **wire 只追加**。`ItemWire` / `AgentEventWire` 的字段只增不改（沿用 `wire.rs` 头注释的既有约定）。

### 4.1 生产侧（谁写 parts）

唯一约定：工具返回的 `Value` 里带一个 `parts` 数组，元素形状即上面的 `RenderPart`。

```json
{ "parts": [ { "kind": "image", "source": { "kind": "artifact", "id": "sha256:..." }, "alt": "render.png" } ] }
```

解析失败 / 不认识 → 该元素丢弃；一个都没解析出来 → 退回 `[Text { 工具输出文本 }]`。**没有任何"如果工具叫 X 就特殊处理"的分支**——`TOOL_LABELS` 之类的名字映射降级为标题 / 图标装饰，不再承担能力。

### 4.2 消费侧（谁画 parts）

`RenderParts.tsx` 是一个 `kind → 组件` 的查表：

| kind | 组件 |
| --- | --- |
| `text` | 既有 `AiMarkdown` |
| `image` | `ImageBlock`（独立块 + 点击全屏预览，见 4.1） |
| `file` | 文件芯片（名字 + 大小，点击下载 / 打开） |
| `link` | 外链 |
| `data` | `schema → 组件` 注册表；未注册 → 退化成 `<details>` 展示 JSON |

### 4.1 图片怎么显示：独立块，不是句子里的字符

三类来源（工具产物、用户的附件、模型回答里的 `![alt](url)`）走**同一个组件** `ImageBlock`
（`components/ai/ImageBlock.tsx`），因为「图片在对话里长什么样」不该取决于它是谁产生的：

- **独立块**：圆角 + 1px 边框、`max-h-80` 限高、`object-contain`，所以一张长截图不会把
  对话顶出屏幕；`lazy` 加载，历史里翻过去的图不抢带宽。
- **点击预览**：`ImageLightbox` 是**应用内**的全屏浮层（`fixed inset-0 z-50`、`bg-black/80`
  + `backdrop-blur`），不是交给系统看图器 —— 后者会把用户从会话里带走。三条关闭路径：点遮罩、
  右上角关闭按钮、`Esc`；**点图片本身不关闭**（那是用户正在瞄的地方）。打开期间锁 `body` 滚动。
- **根节点是 `span` + `block`**，不是 `figure`：markdown 图片渲染在 `<p>` 里，块级元素套进去
  是非法嵌套。

顺带修掉两个旧毛病：

1. `AiMarkdown` 之前用 `disallowedElements={["img"]}` **把 markdown 图片整段丢掉** —— 模型回答
   里写 `![shot](...)` 是空白。现在 `img` 映射到 `ImageBlock`，与 render part 行为一致。
2. 用户消息里的图片之前被画**两遍**：`InlineReferences` 里一个 16px 缩略图 chip，下面
   `RenderParts` 再画一个大图。现在 `InlineReferences` 跳过图片引用（token 照旧消费，chip 与
   token 的 1:1 映射不变），图片只以块出现。

输入框里的图片 chip 保持原样：那是"正在编辑的附件"，不是"消息里的图片"，悬停放大走
`ImageHoverPreview`。

## 5. 数据流

现状（⊥ 为断点）：

```
Tool(Value) --to_string()⊥--> wire.ToolFinished{output:String} --> 前端内存
Message --as_text()⊥--> items（图丢弃）        Message --原样 append⊥--> JSONL（base64 入库）
items --snapshot 无工具输出⊥--> 前端（切会话/重启归零）
```

目标：

```
Tool(Value) --> parts_from_tool_value() --> RenderPart[]
                    ├─> ToolFinished{output, parts}     （实时）
                    ├─> items（还原时从 Message::Tool 重算，不另存 sidecar）
                    └─> Artifact{id} --agent_asset_url--> <img src>（字节带外）
RenderPart[] --> fallback_text() --> TUI / 日志 / cloud web
```

## 6. 阶段

| 阶段 | 内容 | 验收 |
| --- | --- | --- |
| **P0（本轮）** | `RenderPart` 契约 + `fallback_text`；SDK `ToolFinished` **追加** `structured`；工具输出进会话视图（含 `Message::Tool` 还原）；`ItemWire` 追加 `parts`/`output`/`ok`；前端 `RenderParts` 按 kind 分发；TUI 显示 fallback 文本 | 切会话工具输出不丢；TUI 不空白；`cargo test --workspace --features server` 绿；SDK 公开 API 除一处 append 外无变化 |
| **P1（已实现）** | artifact store（`<state>/artifacts/<uuid>.<ext>`）+ `agent_asset_url` 一条命令三 transport（desktop `shirley-asset://` 自定义 scheme / server `GET /api/asset/<handle>` / 前端 `AssetUrlProvider` + `useAssetUrl`）+ jail / mime / 大小 | 工具产出的图在桌面与 cloud web 都能内联显示、点击放大；`.jsonl` 里不含工具产物的 base64 |
| **P2（切片已实现）** | 生产者接入：`read` 命中图片 → `Image`、**`bash` 命令产物自动 → `Image`**、`{content, parts}` 信封约定、产物渲染提到主对话流、cloud 相对路径拼 `baseUrl`；余下：`web_search` → `Link`、`Data` schema 注册表、skill 自带渲染组件 | 新增一种产物只需改工具 / 前端，`session` 与 `wire` 零改动 |
| P3 | 流式产物（边生成边渲染）、产物 GC、跨会话产物复用 | 未排期；P3 不做之前不要提前抽象 |

## 7. P0 落地清单

1. `interface/view.rs`：`RenderPart` / `AssetSource` / `fallback_text` / `parts_from_tool_value` / `parts_from_message_content`；`ChatMessage.parts`、`ToolCallView.output|ok|parts`。
2. SDK `runtime/event.rs`：`AgentEvent::ToolFinished` 追加 `structured: Option<Value>`；`agent.rs` 在成功分支把原始 `Value` 一并送出（错误分支为 `None`）。
3. `interface/session`：`apply_event` 处理 `ToolFinished` → 按 `call_id` 回填；`add_message` / `items_from_messages` 处理 `Message::Tool`（文本还原）与 `Message::User`（图片转 `Image` part）。
4. `desktop/wire.rs` + `web/src/types/wire.ts`：`ToolFinished.parts`、`ItemWire::Message.parts`、`Tools.calls[].output|ok|parts`（全部追加）。
5. 前端：`components/ai/RenderParts.tsx`（kind 分发 + `Data` 退化成 JSON）、工具卡片展示产物、`App.tsx` 把 `parts` 落到 execution。
6. TUI：`ui.rs` 渲染 `ChatMessage` 时把非文本 part 的 `fallback_text()` 追加为独立行。

### P0 落地结果（已实现）

契约、降级、入视图、wire 追加、前端分发、TUI fallback 全部落地，`cargo test --workspace --features server` / `clippy` / `fmt --check` / `tsc --noEmit` 均绿。两处与计划的偏差记在这里：

1. **还原不靠 sidecar。** 原计划用 `<name>.view.jsonl` 旁挂渲染视图；实际实现是**从已有的 `Message::Tool` 重算**（`attach_tool_result` 按 `call_id` 回填）。理由：`Message::Tool` 本来就存了模型看到的那份文本，再存一份渲染视图就是第二个真相源，且 compaction 截断后两者必然漂移。代价：日志里没有 `ok` 状态（`Option<bool>` 为 `None`，前端按"未知"渲染），`Message::Tool` 的文本是 tool 的 `to_string()` 形态，够还原文本、不够还原结构化产物 —— 结构化的部分留给 P2 的产物侧车。
2. **SDK 公开 API 不是零 diff。** `AgentEvent::ToolFinished` 追加了 `structured: Option<serde_json::Value>`（append-only，`wire.rs:9-11` 既有约定）。不改它的替代方案是把 `output` 的解释权搬到应用层，但那需要应用层反向解析 `to_string()` 出来的 JSON 文本，正是本次要消灭的"在不认识 payload 的层里拍平"。**结论：以 append-only 为准，"SDK 公开 API 零 diff"这条验收作废。**

### P1 落地结果（已实现）

**通道形状：不透明 handle，不是路径型 URL。** `agent_asset_url(project, id)` 先经 `ArtifactStore::resolve` 过 jail，再铸一个 uuid v4 句柄登记到 `ProjectRuntimeManager.assets`（`std::sync::Mutex`，因为 Tauri 的 scheme handler 是同步函数；`MAX_ASSET_HANDLES = 512`，超限直接 `clear()` —— 句柄是"页面在屏时的缓存"，不是持久状态，前端重开消息会重新铸）。返回 `shirley-asset://localhost/<handle>`（桌面）或 `/api/asset/<handle>`（server）。三条理由：URL 里不出现项目路径（canonical path 带斜杠难编码）；句柄只能由已认证/本机命令铸造，顺带解决 server 端 `<img>` 无法附加 `Authorization` 的问题；撤销只需清 map。

落地清单：

1. `infra/artifact.rs`：`ArtifactStore`（`put_file` / `resolve`）+ `MIME_TYPES` 白名单（**故意不含 svg / html**）+ `MAX_ARTIFACT_BYTES = 32MB`；`resolve` 先校验 id 形状（32 hex）与扩展名白名单，再 `symlink_metadata` 拒符号链接与非普通文件，最后查大小。7 个单测。
2. `bootstrap.rs`：`AgentFactory.artifacts: Arc<ArtifactStore>`，`assemble_tools(&ArtifactStore)` 把它塞进 `ToolContext`（存裸值，读时 `ctx.get::<ArtifactStore>()` 得 `Arc`）。
3. `tools/read.rs`：命中图片扩展名时 `put_file` 入库并返回 `{"content": "[image] …", "parts": [{"kind":"image","source":{"kind":"artifact","id":…}}]}`；文本路径仍是 `Value::String`（**请求字节不变，prefix cache 不受影响**）；无 store 时降级为只含 `content` 的对象。
4. `interface/runtime`：`asset_handle(project, id)` / `asset_by_handle(handle)`。
5. `interface/desktop/shell.rs`：`register_uri_scheme_protocol("shirley-asset", …)` + `agent_asset_url` 命令；`interface/server`：dispatch 增 `agent_asset_url`，路由增 `GET /api/asset/{handle}`，**挂在 `route_layer(authorize)` 之后且不要求 bearer** —— 句柄本身就是能力。
6. 前端：`bridge.assetUrl(project, id)`（四套 transport 各一处）；`RenderParts.tsx` 的 `AssetUrlProvider`（项目域 + id→URL 缓存）与 `useAssetUrl`（`url` 直出、`artifact` 异步解析、解析失败退回 `fallback_text`）；`ImagePart` 内联缩略图 + 点击在**应用内遮罩**放大（Esc / 点背景关闭，不跳系统看图器）；`App.tsx` 用 provider 包住 transcript，并把用户粘贴图在实时与快照两条路径上都落成 `Image` part。

两处与计划的偏差：

1. **没有 sha256 内容寻址，改用 uuid v4。** `base64` / `sha2` 不是本 crate 的直接依赖，为一个 id 引两个依赖不划算；代价是同一张图重复入库存两份（P3 的产物 GC 再处理）。
2. **产物按项目而非按会话分目录**（`<state>/artifacts/`，没有 `<session>` 层级）。会话名只在项目内唯一，产物按会话分目录会让"跨会话复用已有产物"在 P3 时变成迁移；按项目平铺更简单，且句柄本来就有界。

**验收现状**：`cargo test --workspace --features server`（389 lib + 116 SDK + 集成套件）、`clippy`、`fmt --check`、`tsc --noEmit`、web `npm run build`、`cargo check --features desktop --all-targets` 全绿。

### P2 落地结果：命令产物自动进聊天（已实现）

起因是一个具体场景：让 AI 通过 skill / 脚本生成一张登录二维码 PNG，用户要**在聊天列表里直接扫**。P1 之后这条链路仍然断在三处 —— 只有模型主动 `read` 那张图才会有 `parts`；`parts` 只画在折叠的"过程"里；cloud 模式下相对路径的 `<img>` 会 404。本轮把三处补齐：

1. **模型视图与 UI 视图分流（`{content, parts}` 信封）。** 工具结果同时是"给模型看的文本"和"给人看的产物"，所以工具需要一个能同时说话的形状：`{"content": "…给模型…", "parts": [ …给 UI… ]}`。SDK 的 `value_to_content` 只把 `content` 交给模型（对象且 `content` 为字符串时），`parts` 留在 `ToolFinished.structured` 里透传给 UI。**不这么做的话，模型会读到整坨 JSON 产物描述，既浪费 context 又误导它去解析 UI 的字段。** 文本型工具仍返回 `Value::String`，模型看到的字节不变（prefix cache 不受影响）。
2. **`bash` 是默认生产者。** 命令跑完扫 workspace 根及其一层子目录（跳过 `.git` / `target` / `node_modules` / `dist` / `build`，上限 2000 条目），把 mtime 落在本次执行窗口内（含 2s 余量）、扩展名命中图片白名单的文件入库，最多 3 张，作为 `parts` 附在同一条工具结果上。**理由**：让模型多跑一轮 `read` 才知道"有张图"是浪费，而且模型未必想得到。范围故意很浅 —— 这是"脚本把产物丢在我旁边"的便利，不是爬虫；更深的产物走 `read`。命令没产物时返回值仍是纯字符串。
3. **产物画在对话流里，不画在折叠里。** 新增 `components/ai/ToolArtifacts.tsx`，排在 `AiToolActivityDisclosure` **之前**：一张要扫的二维码藏在两层点击后面等于没生成。工具卡内部不再重复渲染 `parts`（避免同一张图显示两遍），折叠里只留运行日志。
4. **cloud 的 `<img>` 地址补全。** `createHttpBridge.assetUrl` 把 server 返回的 `/api/asset/<handle>` 拼到 `baseUrl` 上（已是 `http` 开头则原样返回）：页面 origin 未必就是 server（remote / cloud 模式），而 `<img src>` 无法携带 bearer token —— 句柄本身就是能力，这点在 P1 已经定过。

新增单测 `attaches_images_the_command_just_produced`（生成的 PNG 变成 `image` part 且引用 artifact 而非字节）与 `leaves_non_image_products_alone`（非图片产物时返回值仍是裸字符串）。

**仍未做**：把粘贴图从 `.jsonl` 搬进产物库（显式非目标）、`Data` schema 的声明层归属、`web_search` → `Link`。

### 非目标（P0 明确不做）

- 不做流式产物（P3）。
- 不改 `Message` / 不给 SDK 加 `Image` 输出变体。
- 不给 `AgentEvent` derive `Serialize`（协议差异继续在 `wire.rs` 收敛）。
- 不用 `assetProtocol` + 全盘 scope（越权面太大，P1 用自定义 scheme + jail）。
- 不把用户粘贴图从 JSONL 里搬走（模型请求体需要它；P1 再评估出站转换）。
- 不做 `Data` schema 的远程声明（只允许本机注册）。

## 8. 风险与回滚

| 风险 | 处理 |
| --- | --- |
| `parts` 让事件/快照变大 | part 只带引用与元信息；`Data` 的 `value` 由生产者自负体积，超限在信号侧截断 |
| 旧前端读新字段 | 全为追加字段，旧端忽略；解析侧未知 kind 静默跳过 |
| 未知 kind 被静默吞掉导致"看起来没渲染" | `fallback_text()` 兜底：宁可显示文本，不可空白 |
| 一次改动牵动三界面 | P0 只改共享视图 + wire 追加；TUI 走 fallback，不改结构；回滚 = revert 单个 commit（无数据迁移） |

---

## 产物泛化：不止图片（已实现）

上一阶段只跑通了「工具产出一张图片」。这一阶段把**通道**做成通用的，让 PDF / PPT / 视频 / 音频 / 压缩包都走同一条路，且生产者是 AI、脚本、专用工具、skill 还是 MCP 都无关紧要。

### 一个通道，mime 决定渲染器

不变的是契约，变的是「这个 payload 长什么样、谁来画」：

| 层 | 不变量 | 变化量 |
| --- | --- | --- |
| 生产 | `{content, parts}` 信封；`artifact::part_for` 是**唯一**构造器 | 谁调用它（工具 / skill / MCP / 模型字节） |
| 传输 | 不透明 handle + jail + mime 白名单 | 白名单里多个后缀 |
| 渲染 | `RenderPart` 五个 kind 不变 | 前端 `FILE_RENDERERS` 按 mime 选组件 |

关键修正：`RenderPart::File` 之前只有 `path`，没有 `source`，**根本进不了资产通道** —— PDF/PPT 连送达都做不到。现在是 `source: Option<AssetSource>`（append-only，`path` 降级为显示提示）。`File` 保持通用，不带 `Pdf`/`Video` 之类变体：支持新格式不该动契约。

### 落地清单

| 文件 | 改动 |
| --- | --- |
| `infra/artifact.rs` | MIME 白名单扩到 office（docx/doc/xlsx/xls/pptx/ppt/odt/ods/odp）、zip、mov/m4a/ogg/flac/heic/bmp；新增 `extension_for_mime`（反查）、`put_bytes(mime, bytes)`（无路径字节：MCP / 远端 API）、`part_for(&Artifact, name, bytes)`（唯一 part 构造器）、`is_attachable(mime)`（人要看的东西）；单产物体积 32MB → 64MiB |
| `interface/view.rs` | `File` 追加 `source`/`path` 可选；`fallback_text()` 不变（TUI / 日志 / 模型请求无需理解新媒介） |
| `tools/bash.rs` | 探测从「新图片」泛化为「新产品」（`is_attachable`，跳过 txt/md/json/csv 这些脚本噪声），改用 `part_for` |
| `tools/read.rs` | `attach_image` → `attach_artifact`：非文本白名单类型（PDF/PPT…）不再报 `binary file; read refused`，而是给出可打开的 file part |
| `web/src/components/ai/RenderParts.tsx` | `FILE_RENDERERS` 注册表：pdf → 内联 iframe + 遮罩全屏 / `video/*` → `<video controls>` / `audio/*` → `<audio controls>` / office → 卡片（浏览器没有渲染器，诚实说明）+ 打开 / 未知 → 通用卡片 |
| `web/src/types/wire.ts` | `file` part 的 `source`/`path` 同步 |

### 四类生产者怎么接

1. **AI / 脚本（默认）**：`bash` 跑完自动探测工作区根 + 一层子目录的新产物（上限 3 个、跳过 `.git`/`target`/`node_modules`/`dist`/`build`），无需模型额外动作。
2. **专用工具**：返回值写 `{"content": "...", "parts": [artifact::part_for(...)]}` —— 注意 `content` 里**不要**塞 base64，模型只看 `content`。
3. **skill**：无需新接口，走 1 或 2；`SKILL.md` 里的脚本产物被同一次探测捞到。
4. **MCP（接缝已备，客户端未实现）**：MCP 的 `{type:"image"|"audio"|"resource", data:"<base64>", mimeType}` 解码后交给 `ArtifactStore::put_bytes(mime, bytes)`，再用 `part_for` 拼同样的信封。`put_bytes` 按 mime 反查扩展名，所以 MCP 报的 mime 决定它落成什么类型，且**只有白名单内的才收** —— 远端无法用自造的 mime 换取一个渲染器。

### 边界

- 白名单不含 `svg`（脚本注入面）；`html` 收录，但**只以文本递送 + 沙箱 iframe 渲染**，见下一节；office 给卡片而非预览（没有解析器，假装能预览是骗用户）。
- 单产物 64MiB 上限，且资产通道目前**整文件读取**（无 `Range`/流式）—— 视频拖动进度条、超大文件是已知未做项。

## 可交互：产物不只是「看得见」

前面的通道解决「能看见」；这一节解决「能操作」。差别不在渲染器多不多，而在**产物要不要执行**：图片、PDF、视频是浏览器自带的解码器在干活，HTML 则要跑它自己的脚本、响应点击。执行是另一类问题 —— 一旦允许跑，就必须先回答「它跑在哪个源上」。

### 决策：HTML 不走 URL，走文本 + `srcdoc`

直觉做法是把 HTML 也塞进 `shirley-asset://` 那样的 URL，然后 `<iframe src=...>`。这条路我们不采用，三个理由：

1. **同源即同权**。用 URL 加载，iframe 的源就是应用的源，生成出来的页面能读 `localStorage`、能拿到 bearer token、在桌面端还能碰 Tauri IPC。等于把「工具产出一段代码」升级成「工具在你的应用里执行任意代码」。
2. **自定义 scheme 在 iframe 内不可靠**。桌面端 `shirley-asset://` 是 Tauri 自己注册的协议，子框架加载要跨一层 webview 的资源解析，行为随平台/版本漂移。
3. **cloud 有 CORS**。cloud web 的 origin 是页面，资产在 server 上，`<iframe src>` 拿不到带 `Authorization` 的响应头 —— 和 `<img>` 同样的老问题，只是 `<img>` 我们早就是靠拼 `baseUrl` 绕过去的，iframe 绕不过去。

所以新增一条只读文本命令 `agent_asset_text { project, id }`：

```
runtime::asset_text(project, id)
  → 走产物 jail 重新解析 id（不信任渲染层）
  → 仅当 jail 判定 mime 是 text/*（is_textual）才读盘，否则报错
  → MAX_ASSET_TEXT_BYTES = 2 MiB 截断，返回 { text, mime, truncated }
```

两条通道并列、各管一类：`agent_asset_url` 给「解码器干活」的二进制，`agent_asset_text` 给「要执行」的文本。两条都要 `project` + jail 校验，都无法靠伪造 id 取到 jail 外的文件。

### 前端：源不透明，能力显式列

```tsx
<iframe
  srcDoc={html}
  sandbox="allow-scripts allow-forms allow-modals allow-popups"
/>
```

关键在于**没有** `allow-same-origin`：`srcdoc` + 这个 sandbox 组合让文档落在一个**不透明源**上。效果是：

- 脚本、表单、`alert`/`confirm`、`window.open` 都能用 —— 图表库、交互看板、小计算器都能真跑；
- 但它是「另一个站点」，读不到父页面的 DOM、`localStorage`、token，也调不到 Tauri IPC；
- 反过来也没有 `allow-top-navigation`，它无法把用户导航走。

这是「可交互」与「不可信代码」唯一能共存的位置：能力显式列在 `sandbox` 里，没列的就没有。

### 注册表：变化的只有 kind → component

`FILE_RENDERERS` 是本地注册表，`match(mime) → component`，首个命中优先，未知 mime 落到通用下载卡片。当前顺序：

| mime | 渲染器 | 交互性 |
| --- | --- | --- |
| `text/html` | `HtmlFile`（`srcDoc` + sandbox iframe，内联 `h-96`，可放大遮罩） | **真执行** |
| `application/pdf` | `PdfFile`（iframe 预览） | 翻页/缩放由浏览器 PDF 阅读器提供 |
| `video/*` `audio/*` | `<video>` / `<audio controls>` | 播放/拖动（整文件，无 `Range`） |
| office 三种 | 卡片 + 打开，标注「需本地应用」 | 无 |
| 其他 | 通用下载卡片 | 无 |

`DATA_RENDERERS` 同理：外部只能送 mime + data，**不能注册组件**，也不能用远端临时编的 schema 名。这条约束是信任边界的一部分 —— 加了 HTML 执行能力之后更不能松。

### 代价与已知缺口

- 文本通道有 2 MiB 上限，超出截断并在 `truncated` 里说明；一个要跑的页面被砍一半是跑不起来的，所以模型应当写自包含的单文件页面，而不是把资源散在别处。
- `srcdoc` 意味着页面**无法相对路径引用同目录资源** —— 这是设计使然（那需要给它一个真实的源），自包含是硬要求。
- 沙箱内没有网络隔离之外的写权限约束可表达：它不能写盘（没有 API），但能发同源外的请求。若将来要收紧，需要 CSP 注入而不是 `sandbox`。
- office 仍无预览；策略是 prompt 引导「需要用户看就产 HTML/PDF」。

## 入向：用户发来的媒体先进库

出向（模型/AI 产物 → 用户）与入向（用户 → 模型）走的是**同一个资产库**，这就是「library」这层东西的意义：一份 `ArtifactStore`、一套 MIME 白名单、一个大小上限、一个 jail。

入向的链路是「先落库，再引用」，而不是「把字节塞进消息」：

1. 前端把媒体交给 `agent_send`（当前仍是浏览器粘贴得到的 `data:` URL）。
2. harness 解码后经 `ArtifactStore::ingest_data_url` 落库，得到一个 id 和一个可改的名字（`<id>.meta.json` sidecar）。
3. 用户消息里**没有字节**：文本追加一行 `name (mime, size) id=<id>`，图片另外带一个 `ImageSource::FileId` part（应用层据此渲染卡片/缩略图）。
4. 模型侧看到的是**只有文本**——`resolve_image_refs` 在组装请求时对 `User` 消息丢弃 `FileId` part，只对 `Tool` 结果解析成 `data:` URL。因此模型必须自己决定要不要看：它调 `media`（`list` / `info` / `read`），图片通过**工具结果里的 image part** 回到上下文。

这条不对称是刻意的，也是「模型自己决定读内容」的强制点：

| 位置 | `file_id` 如何对待 | 理由 |
| --- | --- | --- |
| `Message::User` | 丢弃（文本保留） | 用户投递 ≠ 模型已看；否则等于强行把图塞给模型 |
| `Message::Tool` | 解析成 `data:` URL | 这是模型主动要的，才是它该看到的 |

代价与边界：

- 一次「看图」要多一个来回（先 `media`，再回答）。这是用延迟换上下文控制权，对长会话有利。
- 解析失败（文件被删、超过 8 MiB 内联上限）时丢弃该图片 part，文本保留——会话不会因为一个缺失的附件整轮作废。
- `media` 对 PDF / office / 视频**诚实降级**：只描述、不假装读过。假装读过 PDF 正是 Agent 开始编造内容的起点。
- `read` 的文本分支有 32 KiB 截断；要更多可以再用 `read` 按行区间读。
- 大文件与视频仍是整文件（无 `Range`），单产物上限 64 MiB。
