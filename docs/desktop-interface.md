**Shirley 技术方案 · 桌面界面（Desktop Interface）**

> **状态：方案期（尚未实现）。** 本文是迁移 shiwen 聊天 UI 为 Shirley 桌面界面的技术方案。
> **已确认决策**：形态 1（Tauri 2 + React/TS，直接 copy）/ 与 TUI 并存 / 允许引入 Node 构建链 / 审批流保留但第一期不展示。
> 落地后请把「实现落地」一节补上，并同步 `docs/README.md` / `Agent.md` 的文档索引。
>
> 参考源：`/Users/sean/Desktop/repo/shiwen-open-source`（下称 **shiwen**）。

---

**一、这份文档要解决什么**

用户喜欢 shiwen 的**聊天交互与 UI**（不是它的业务），希望在 Shirley 里新增一个界面，用 Tauri 把 shiwen 的聊天 UI 迁移过来，去掉其中的 shiwen 定制逻辑。

Shirley 现在只有一个界面：`src/interface/` 下的 ratatui TUI。本方案新增第二个界面——一个 Tauri 桌面窗口 + React 前端——与 TUI **并存**，两者**共享同一套底层（LCA）**。

---

**二、关键结论：是「兄弟界面」，不是「TUI 上叠一层 web」**

一个必须先澄清的形态问题（用户原话在问）：

- ❌ **不是**「TUI 上多一层 web」。ratatui 会接管终端（alternate screen / raw mode / 鼠标捕获），webview 是独立原生窗口，**两者不能同时画同一份对话**；且 `interface/` 里全是 TUI 专属逻辑（`tui.rs` 事件循环、`ui.rs` 的 `MessageCache`/换行估算、`selection.rs` 鼠标选区、`markdown.rs` 转 ratatui `Line`/`Span`），对 web 毫无用处。
- ✅ **是**「两个并列界面，共享同一个 LCA」。TUI 与 desktop 是**同一份 `Agent` 的两个消费者**，各自渲染，互不干扰。

```
                    ┌───────────────────────────────┐
                    │  main.rs（装配层，唯一入口）      │
                    │  settings / session / tools /   │
                    │  prompt / model_config          │
                    └───────────────┬─────────────────┘
                                    │ 构造出 Agent + catalogs
                    ┌───────────────┴─────────────────┐
                    ▼                                 ▼
        ┌───────────────────────┐        ┌───────────────────────┐
        │  interface/ (TUI)     │        │  interface/desktop/    │
        │  ratatui 渲染          │        │  Tauri 壳 + React web  │
        └───────────┬───────────┘        └───────────┬───────────┘
                    │ 消费 AgentEvent               │ 消费 AgentEvent
                    └───────────────┬─────────────────┘
                                    ▼
                    ┌───────────────────────────────┐
                    │  shirley-agent-sdk::Agent      │
                    │  run_stream() → AgentEvent 流   │
                    └───────────────────────────────┘
```

**LCA（公共逻辑）= SDK 的 `Agent`（`run_stream()` → `AgentEvent` 流）+ `main.rs` 的应用装配。** `AgentEvent` 就是那个公共接口：TUI 在 `tui.rs::apply` 里消费它，desktop 在 Tauri command 里消费它、转成 webview 事件。**两边对接的是同一个事件流，不是同一套渲染。**

---

**三、共享边界（要定死的东西）**

| 层 | 归属 | 说明 |
| --- | --- | --- |
| `src/settings.rs` / `session.rs` / `models.rs` / `prompt.rs` / `tools/` | **共享** | crate 根下的模块，与界面无关，两个界面都用 |
| `src/main.rs` 的装配 | **共享，需小改** | 现在是「装配完直接 `interface::run`」，要抽成 `Bootstrap` 并按模式分派 |
| `shirley-agent-sdk`（`Agent` / `AgentEvent`） | **共享** | 不动 |
| `src/interface/`（tui/ui/app/update/event/selection/markdown/command） | **TUI 专属** | 原样保留 |
| `src/interface/desktop/`（Tauri 壳 + `web/`） | **desktop 专属** | 新增 |

**两处需要先做的重构**

1. **`main.rs` 抽 `Bootstrap` + 分派**。把现在 `main.rs` 第 26–104 行的装配（settings / models catalog / tools / model_config / session / Agent）抽成一个 `Bootstrap` 结构，两个界面都能拿到同一个 `Agent` + catalogs。**不是复制装配，是真的共享。**
   ```rust
   let bootstrap = Bootstrap::assemble(working_dir)?;
   match mode {                                  // 来自 --desktop / 环境变量
       Mode::Tui     => interface::tui::run(bootstrap).await,
       Mode::Desktop => interface::desktop::run(bootstrap).await,
   }
   ```
2. **slash 指令归属**（`src/interface/command.rs` 的 `/login` `/model` `/session` `/rewind`）。这些是应用级行为，desktop 也需要。**第一步先不共享**：desktop 先只做渲染与收发，指令留到后续里程碑再下沉为共享 `commands` 模块。

---

**四、迁移范围：copy 什么，删什么**

**原则：最小迁移成本——关键代码直接 copy，再接 SDK。**

### 4.1 要 copy 的（shiwen 路径 → Shirley 落点）

| shiwen 源 | 作用 | 处置 |
| --- | --- | --- |
| `apps/web/src/lib/components/ai/AiConversationSurface.tsx` | 会话外壳（transcript + composer） | copy |
| `apps/web/src/lib/components/ai/AiConversationTranscript.tsx` | 消息列表 + 自动滚动 + 头像 | copy（去掉 minimap） |
| `apps/web/src/lib/components/ai/AiChatComposer.tsx` | 输入框（自适应 / Enter 发送 / 停止） | copy |
| `apps/web/src/lib/components/ai/AiMessageTimeline.tsx` | 文本块与工具卡按 `content_offset` 交错 | copy |
| `apps/web/src/lib/components/ai/AiMarkdown.tsx` | 流式 markdown（基于 `streamdown`） | copy（去掉 citation 插件） |
| `apps/web/src/lib/components/ai/AiAssistantAvatar.tsx` | 助手头像 | copy（换 Shirley 形象或留占位） |
| `apps/web/src/lib/components/ai/AiToolExecutionCard.tsx` / `AiToolActivityDisclosure.tsx` | 工具执行卡片 / 活动折叠 | copy（审批链路代码保留，渲染第一期不展示） |
| `apps/web/src/lib/chat-input.ts` | Enter 发送判定 | copy |
| `packages/ui/src/chat.tsx`（`Message` / `Bubble` / `MessageScroller` / `useMessageAutoScroll`） | 聊天基础原语 | copy |
| `packages/ui/src/tokens.css` | Tailwind 4 设计 token | copy |
| `packages/ui/src/utils.ts`（`cn`） | class 合并 | copy |
| 必要的 `packages/ui/src/{button,textarea,scroll-area,...}` | 依赖原语 | 按需 copy |

### 4.2 要剥离的（shiwen 定制逻辑，**迁移前先确认**）

| shiwen 专有 | 为什么只属于 shiwen | 处置 |
| --- | --- | --- |
| **A2UI surfaces**（`A2uiTurnSurface` / `A2uiClientAction` / `AiQuestionSurface`） | shiwen 自有的服务端驱动结构化 UI 协议 | 删 |
| **实体提及 / chips**（`EntityChips` / `EntityMentionPopover` / `useEntityMentions`） | article/space/document/material——shiwen 知识库 | **迁移其交互骨架，对象改绑工作区文件**（见 4.3） |
| **引用**（`[[citation:id]]` 插件 / `inlineCitationMarkdown` / `StreamCitations`） | 绑定 shiwen 检索 + 实体 URL | 删 |
| **工具审批流**（`approval_state` / `onToolDecision` / 卡片上的 approve/reject） | shiwen human-in-the-loop 关卡；Shirley 暂无权限层 | **保留代码，第一期不展示**（用户已确认） |
| **实体/空间/文章锚定**（`host_kind` / `promoted_space_id` / `host_repository`） | shiwen 工作区模型 | 删 |
| **会话分享**（`AiConversationShare` / 公开分享） | shiwen 功能 | 删 |
| **OCR / 素材上传**（`fetchMaterialOcrStatuses` / `handleMaterialUploaded`） | shiwen 桌面应用 | 删 |
| **桌面工作区切换器**（`useDesktopWorkspaces` / `DesktopScreen`） | shiwen 桌面登录/工作区 | 删（Shirley 不需要） |
| **SSE 线格式**（`packages/ai-session` 的 `parseAiStreamEvent` / `streamAiMessage`） | shiwen 服务端协议 | **重绑**到 Shirley 的 `AgentEvent`（见第六节） |
| `AiConversationMinimap`（轮次缩略图） | 依赖 shiwen 消息结构，非核心 | 可删（第一步先删，后续想要再加） |
| `lucide-react` 之外的 shiwen 依赖（tiptap / blocknote / pdfjs / tesseract 等） | shiwen 文档/OCR 栈 | 不引入 |

> **确认点**：上表每一条都需用户签字。
>
> **已确认（用户）**：
> - **审批流保留代码，第一期不展示**——`approval_state` / `onToolDecision` 的数据结构与回调链路先原样迁入，渲染层第一期不画审批按钮；后续接权限层时再启用。
> - **`@` 引用保留交互、对象重绑工作区文件**（见 4.3）——不再按"entity 概念一律删"处理。
> - 其余条目默认全部删除。

### 4.3 `@` 引用（迁移 + 重绑，**已落地**）

shiwen 的 `@` 逻辑（输入 `@` 触发候选浮层、选中生成 chip、`EntityChips` 回显）交互骨架值得留，
但它绑定的对象是 shiwen 知识库实体（article/space/document/category/conversation/material）。
Shirley 里真实存在的可引用对象只有**工作区里的文件与目录**，故**重绑**而非照搬：

| shiwen 源 | Shirley 落点 | 变化 |
| --- | --- | --- |
| `lib/entity-mentions/useEntityMentions.ts` | `web/src/lib/file-mentions/useFileMentionSearch.ts` | 对象换成 `FileReference`；检索走 Rust 侧 `agent_search_files`（本地文件系统），不再分页 / cursor；**只保留无头搜索控制器**（浮层状态 + 异步检索），输入区 DOM 归编辑区管 |
| `lib/entity-mentions/EntityChips.tsx` | `web/src/lib/file-mentions/FileChips.tsx` | 图标收敛为 文件 / 目录；去掉 material OCR / 删除资料；`FileChip`（单个，可内联）+ `InlineReferences`（正文按占位符与引用交错） |
| `lib/entity-mentions/EntityMentionPopover.tsx` | `web/src/lib/file-mentions/FileMentionPopover.tsx` | 去掉分组（article/space/…）、OCR 徽标、分页「加载更多」；只留结果列表 + 键盘选择（搜索框由编辑区承载，`showSearch=false`） |
| `lib/entity-mentions/entityLoader.ts` | Rust `src/workspace_search.rs` + `agent_search_files` | 检索落到应用层（工作区遍历 + 关键词排序），不经 SDK |

- **引用如何进模型**：`@` 只作用于界面——选中后在光标处插入一个内联 chip，发送时
  由 `shell.rs::compose_prompt` 把引用路径拼进**这一轮**的 prompt。**引用不是 Agent 的对外
  契约**：`Message` / `AgentEvent` 都不动，SDK 未因此新增任何对外类型（见第八节验收第 4 条）。
- **检索边界**：只在工作区根下遍历，跳过 `.git` / `node_modules` / `target` / `dist` 等，
  条目数设上限；这与 `read_file` 工具的工作区约束同源，但**在应用层实现**，不碰 SDK。

#### 4.3.0 内联混排：`contenteditable` 富输入（**已落地**）

初版把引用做成「正文外的 `references` 数组 + 输入框顶部的 `FileChips` 块」：chip 与文字
**分属两层**，视觉上永远堆在正文上方，无法「文字 + tag 同行」。根因是输入控件是 `<textarea>`——
它只能渲染纯文本，**无法承载内联节点**。修复即把输入区换成 `contenteditable` 富输入：

- **正文 = 一条带占位符的字符串**。编辑区里 chip 是 `contenteditable=false` 的内联 `<span>`；
  序列化时写成 `U+FFFC`（OBJECT REPLACEMENT CHARACTER）占位符。于是「文字 + 引用」仍是一条
  有序字符串：**第 N 个占位符 ↔ `references[N]`**。发送前用 `stripTokens` 剥掉占位符，
  引用仍走既有 `references` 数组（Rust `compose_prompt` 一行没改）。
- **编辑期不重渲染 DOM**（否则光标必丢）。React 只渲染编辑区根 `<div>`，内部内容由
  `MentionEditor` 命令式维护；普通输入交给浏览器原生编辑，`onInput` 只做序列化上报。
  仅在**结构变化**（插入 / 删除 chip、外部重置 / 清空）时 `rebuild` 整段 DOM，并用
  `pendingCaret` 恢复光标。
- **键盘整块删 chip**：命中 chip 占位符的退格 / 前向删除，手写删除该 chip 节点 +
  对应 `references[i]` + 重建（否则浏览器删的是字符、引用数组会错位）。
- **`contenteditable=false` 的光标死角（已修）**：chip 是不可编辑的内联节点，浏览器既
  不把光标放进「chip 与相邻文字之间」，方向键也跨不过它，点击落点也不可靠。三处补齐：
  ① 方向键跨 chip 边界时手动 `setCaretAt`（普通文字仍走原生）；② 点击 chip 按落点左/右
  半区把光标吸附到 chip 前 / 后；③ 退格 / 前向删除在占位符处整块删。统一用
  「序列化文本偏移 ↔ DOM 位置」（`caretOffset` / `setCaretAt` / `chipOffset`）换算。
- **用原生 `beforeinput`，不用 React 的 `onBeforeInput`（已修）**：React 18 的
  `onBeforeInput` 是 `textInput`/`keypress` 合成的旧接口，`nativeEvent.inputType` **恒为
  `undefined`**——原先基于它的删除 / 换行分支全是死代码（这正是「删除失灵」的根因）。
  改为在编辑区根节点上 `addEventListener("beforeinput")`：只有原生事件能拿到 `inputType`
  且 `preventDefault` 能阻止默认编辑。
- **发送后回显**：用户消息的 `content` 保留占位符，渲染时走 `InlineReferences` 按占位符与
  `message.references` 交错，chip 落回它在正文里的原始位置——不再堆顶部。
- **实现落点**：`web/src/lib/file-mentions/` 下 `editor-dom.ts`（序列化 / 光标 / 重建 DOM）、
  `MentionEditor.tsx`（富输入）、`useFileMentionSearch.ts`（无头检索）、`FileChips.tsx`
  （`FileChip` / `InlineReferences`）。`AiChatComposer` 新增 `inputSlot`，传入时替换内置 textarea。
  `src/App.tsx` 用 `input` + `references` 两个 state 接线，`send()` 判空用 `stripTokens`。

### 4.3.1 markdown 渲染对齐（**已修**）

`AiMarkdown.tsx` 与 shiwen **逐字节相同**（只差 `cn` 的 import 路径），所以渲染不一致**不在组件**，
在 **CSS**：Shirley 的 `styles/index.css` 漏了 shiwen 的那行 `@source`。

Tailwind 4 默认**不扫描 `node_modules`**。streamdown 把 markdown 各块级元素的 utility 类
（`mt-6` / `text-2xl` / `border-border` / `text-muted-foreground` / `divide-y` / `wrap-anywhere` …）
写在它打包后的 JS 字符串里；不显式告诉 Tailwind 去扫这份 JS，这些类**一个都不会被编进产物 CSS**，
markdown 就以「无样式」渲染（标题不放大、列表没间距、代码块没边框/底色、引用块没竖线）。
shiwen 的 `index.css` 有 `@source ".../node_modules/streamdown/dist/*.js"`，Shirley 迁移时漏掉。

修复：在 `web/src/styles/index.css` 补回该 `@source`（路径按 Shirley 的 `web/` 层级写成
`../../node_modules/streamdown/dist/*.js`），并补齐代码块滚动条的 `-track` / `-thumb:hover` 两条
（对齐 shiwen `globals.css`）。**改前端目录结构时，这行 `@source` 的相对路径要跟着调。**

### 4.3.2 聊天区固定宽度 + 连续工具调用收集（**已对齐**）

shiwen 的 `AiConversationSurface` 把**转写区 + 输入框**一起包在
`<section className="mx-auto flex h-full w-full max-w-190 min-h-0 flex-col">` 里
（`max-w-190` = `calc(var(--spacing) * 190)` = 760px）。Shirley 原先没这层包裹，聊天区随窗口拉满。
修复：`App.tsx` 补上同款 `<section className="mx-auto w-full max-w-190 min-h-0 flex-1 flex-col">`
包裹 `AiConversationTranscript` + 错误行 + 输入框，聊天区与输入框都固定宽度居中。

「连续工具调用收集到一起」：shiwen 的 `FreeChatAssistantMessage` 只把**用户可见**的执行项
（`pending` / 审批中 / 带 `entity_url` 的完成项）单独成卡，其余**全部**塞进一个
`AiToolActivityDisclosure` 折叠区，渲染成一行「查看处理过程 · N 项」。Shirley 没有 entity / 审批渲染，
所以执行项都归入折叠区，连续的工具调用被收成一行，而不是每个调用铺一张卡。
`AiToolActivityDisclosure` 内部仍按 `content_offset` 排序、并对连续失败做聚合（`toolErrorUtils`）。
**折叠区的落点见 4.3.4**：不再全部堆在正文顶部，而是作为线性段落流中的一段，按事件顺序与正文/思考交错。

### 4.3.3 markdown 渲染对标 TUI（**已落地**）

4.3.1 补 `@source` 只解决了「utility 类没被编进产物」——streamdown 的默认样式本身
仍偏「营销页」气质（`text-3xl` 标题、`bg-muted` 行内代码、`my-4`/`space-y-4` 大间距），
而且它依赖一组 **shadcn 式 token**，Shirley 的 `tokens.css` 并不完整。所以
「desktop markdown 不如 TUI 优雅」是**两层问题**，4.3.1 只修了第一层。本小节修第二层：
**不再追求与 shiwen 逐像素一致，而是直接对标 TUI（`src/interface/markdown.rs`）的排版与配色**。

三个具体 bug（都已修）：

1. **`--color-sidebar` 未定义**。streamdown 把代码块 / 表格外层容器画成 `bg-sidebar` +
   `border-sidebar`，Shirley 的 `tokens.css` 没有这一项 → 这些类解析为空，容器**没底色、没边框**。
   修复：`tokens.css` 补 `--color-sidebar`（light `#f1f1ee` / dark `#20201e`，取 inset 同系）。
2. **`bg-muted` 语义冲突**。Shirley 把 `--color-muted` 当**文本灰**（`#676763`），
   而 streamdown 拿 `bg-muted` 当**浅色底**（行内代码、表头）→ 变成「深灰底 + 深色字」，对比度极差。
   修复：在 `index.css` 里把行内代码改回浅色 inset 底 + 细边框（`[data-streamdown="inline-code"]`），
   表头底色同理。
3. **标题被 `twMerge` 放大**。`AiMarkdown.tsx` 的 `h1/h2/h3` 覆写用
   `cn("... text-title-sm ...", className)`，streamdown 传入的 `text-3xl`/`text-2xl` 在**后**，
   `twMerge` 后置者胜出 → 标题过大。修复：不靠 `twMerge` 斗优先级，直接在
   `index.css` 用 `[data-streamdown^="heading-"]` 逐级收敛字号（H1 1.25rem → H4+ 0.8125rem），
   并把 H4–H6 降为 muted 色。

其余对齐（写在 `web/src/styles/index.css` 的 `[data-ai-markdown]` 作用域内，**不影响应用其它 UI**）：

| 元素 | TUI（`markdown.rs`） | desktop 覆盖 |
| --- | --- | --- |
| 块间距 | 块间约一行 | `[data-ai-markdown] > * > * + *` 统一 `0.75rem`（清掉 `my-4`/`mt-6`） |
| 行内代码 | 黄色前景 | 浅色 inset 底 + 细边框，同号字 |
| 代码块 | 边框 + 语言标签 + 暗色 | 外层 `sidebar` 底 + `line` 边框；body 用 `canvas` 底 |
| 引用块 | `│` 竖线 + 斜体 + 暗色 | `border-left: 2px line-strong` + italic + muted |
| 表格 | `│` / `─┼─` 网格 | 表头 `inset` 底、单元格 `line` 边框 |
| 链接 | 蓝色下划线 | `--color-link` + underline |
| 分割线 | `─` 暗色 | `line` 色、`0.75rem` 上下距 |

> 这些规则**未放进 Tailwind layer**（写在 `@layer utilities` 之后），因此能稳定压过
> streamdown 编进 utilities 层的默认类——这是「用 CSS 覆盖第三方组件默认样式」的常规做法，
> 不依赖 `twMerge` 的先后顺序。改 `AiMarkdown.tsx` 的 `BASE_COMPONENTS` 时记得：
> 组件层字号会被这里的 CSS 再盖一次，两者要一起看。

### 4.3.4 一轮回复按事件顺序线性渲染 + 思考折叠（**已落地**）

对标 TUI 的**线性消息流**。TUI（`app.rs`）把一轮回复拆成有序的 `Item`：思考块 → 正文 →
工具组（`Item::Tools`），**按事件发生顺序**排列，可以出现「思考 → 正文 → 工具 → 正文 →
思考 → 工具」这种交错。desktop 原先相反：`AssistantMessage.tsx` 把该轮**所有** `executions`
合成**一个** `AiToolActivityDisclosure`，挂在 `contentOffset = executions[0]?.content_offset`
上——而 `content_offset` 根本没被填充（恒 `undefined` → 0），于是**所有工具调用都堆在正文
顶部**；`reasoning_delta` 更是被 `App.tsx` 直接 `break` 丢弃。

> **走过的弯路**：第一次修只是给 `tool_started` 补上 `content_offset`（正文码点偏移），
> 让 `AiMessageTimeline` ���偏移把工具插进正文。但用户实测仍「所有工具堆在一处」——
> 因为一轮里多个 `tool_started` 是**连续**发生的，它们的 offset 相同，于是又归并到同一
> 折叠区；而且「思考块 / 工具块本身是流中的独立段落」这件事，**偏移模型根本表达不了**。
> 结论：不要用「正文偏移」去定位工具，而要把一轮回复直接切成**线性段落数组**。

**最终模型：线性 segments（对标 TUI 的 items 数组）。** `types/ai.ts` 定义
`AiSegment = { kind:"content"; text } | { kind:"reasoning"; text } | { kind:"tools"; executions }`。

- **`App.tsx`** 持有 `turnSegments: Record<assistantId, AiSegment[]>`。每个事件到达时
  `appendSegment`：**连续同类型的增量并进同一段，类型一换就新开一段**（对标 TUI
  `append_streaming_delta`）。于是数组顺序天然是事件顺序：
  `reasoning_delta` → reasoning 段、`content_delta` → content 段、`tool_started` → tools 段。
  `tool_finished` 只**原地更新**已有 tools 段里的对应执行项（不新开段）。
- **`AssistantMessage.tsx`** 拿到 `segments` 后**按数组顺序**渲染，`gap-3` 分隔：
  `content` → `AiMarkdown`；`reasoning` → `ReasoningDisclosure`（折叠区）；`tools` →
  `AiToolActivityDisclosure`（折叠区）。`streaming` 只标给**最后一段**（正在增长的那段）。

**思考折叠区**：`@/ui/collapsible` + `Brain`/`ChevronDown` 图标，收起时一行「思考过程」
（流式中显示「正在思考」），展开显示完整思考文本（`border-l-2` 竖线 + muted 色，呼应 TUI 的
暗色斜体思考块）。默认**收起**，不喧宾夺主。

**连续工具收集**仍保留（见 4.3.2）：同一 tools 段内的执行项收进一个
`AiToolActivityDisclosure`，渲染成一行「查看处理过程 · N 项」。

> 旧的偏移模型 `AiMessageTimeline` 现已无引用（`App.tsx` / `AssistantMessage.tsx` 都不再用），
> 保留文件未删（Vite tree-shake 掉，不影响 bundle）。它若将来还要用，需先把 `content_offset`
> 真正填对——但线性 segment 模型已经能覆盖「交错」需求，`AiMessageTimeline` 大概率不再需要。

### 4.4 要保留的（用户真正喜欢的「聊天交互和 UI」）

- `AiChatComposer`：自适应增高 textarea、Enter 发送 / Shift+Enter 换行、发送 / 停止按钮。
- `AiMessageTimeline`：按 `content_offset` 把文本块与工具卡按时间线交错。
- `AiMarkdown`：流式 markdown 渲染（`streamdown`，`parseIncompleteMarkdown`）。
- `useMessageAutoScroll`：跟随最新（离底才停止跟随）。
- 工具执行卡片 / 活动折叠的视觉。
- 聊天区固定宽度（`max-w-190`，见 4.3.2）。
- 连续工具调用收集成折叠区（见 4.3.2）。
- `tokens.css` 的暖灰设计语言。

---

**五、技术选型**

| 维度 | 选择 | 理由 |
| --- | --- | --- |
| 桌面壳 | **Tauri 2** | 与 shiwen 一致；Rust 后端天然能直接调 `Agent` |
| 前端 | **React 18 + TypeScript + Vite + Tailwind 4** | 忠实复刻 shiwen 交互，形态 1 |
| 包管理 | **npm**（独立 `package.json`，放在 desktop 前端目录） | shiwen 用 npm；不与 Rust workspace 冲突 |
| markdown | `streamdown` | shiwen 同款，流式渲染体验一致 |
| 原语 | `@radix-ui/*` + `lucide-react` + `class-variance-authority` + `tailwind-merge` + `clsx` | shiwen `@shiwen/ui` 的依赖集，按需最小引入 |
| 前后端通信 | Tauri **command + event** | 前端 `invoke` 发消息，后端 `emit` 推 `AgentEvent` |

**代价（需用户知情）**：本方案会给这个**纯 Rust 仓库引入 Node/npm/Vite 构建链**。这是形态 1 的固有代价；若不可接受则退回形态 2（Rust 原生 webview + 手写前端），但那样无法复刻交互。

---

**六、SDK 接入：把 shiwen 的流换成 Shirley 的 `AgentEvent`**

这是「接入我们的 SDK 消费」的核心。shiwen 前端消费的是 SSE（`parseAiStreamEvent` → `onThinking` / `onDelta` / `onTool` / `onCitations` / `onA2ui` / `onDone`）。Shirley 侧把它替换为对 `AgentEvent` 的消费：

| Shirley `AgentEvent` | 前端行为 |
| --- | --- |
| `ContentDelta(text)` | 追加到当前 assistant 消息正文（`onDelta`） |
| `ReasoningDelta(text)` | 追加到思考区（`onThinking`） |
| `MessageAdded(msg)` | 落一条消息（assistant 落库时结束流式态） |
| `ToolStarted { call_id, name, arguments }` | 插入「运行中」工具卡（含调用参数） |
| `ToolFinished { call_id, name, ok, output, elapsed_ms }` | 把工具卡置为完成 / 失败 |
| `Usage(usage)` | 更新页脚 usage / 缓存命中率 |
| `ContextUsage { used, limit }` | 更新上下文占用 |
| `CompressionStarted` / `CompressionFinished` | 压缩状态提示 |
| `Finished(RunResult)` | 结束本轮（对应 `onDone`） |
| `Err(AgentError)` | 错误态（对应 `onError`） |

**Rust 侧桥接**（示意）：
```rust
#[tauri::command]
async fn agent_send(state: State<'_, DesktopState>, app: AppHandle, text: String) -> Result<(), String> {
    // Agent 放 Option：运行期间 take 出去移入后台任务，结束后放回
    // （与 TUI 同款 take/restore 手法，避免并发驱动同一 Agent）。
    let mut agent = state.take_agent().ok_or("agent 正在运行中")?;
    tauri::async_runtime::spawn(async move {
        let mut stream = agent.run_stream(&text);
        while let Some(event) = stream.next().await {
            let wire = match event {
                Ok(event) => AgentEventWire::from_event(event),
                Err(error) => AgentEventWire::Error { message: error.to_string() },
            };
            let _ = app.emit("agent://event", wire);   // 序列化成前端可消费的 JSON
        }
        state.restore_agent(agent);
    });
    Ok(())
}
```
前端 `listen("agent://event", ...)` 后按上表 dispatch 到 store（见 `web/src/lib/bridge.ts`）。

**Tauri commands 一览**（`src/interface/desktop/shell.rs`）：

| command | 作用 |
| --- | --- |
| `agent_send { text }` | 发起一轮 `run_stream`，事件经 `agent://event` 推送 |
| `agent_model_name` | 当前模型名（页脚 / 选择器展示） |
| `agent_list_models` | 列出可选模型（转发 `ModelCatalog`） |
| `agent_set_model { model }` | 热切换模型（`Agent::set_model`，不重建 Agent） |
| `agent_cancel` | 预留（SDK 取消机制未落地，见 `Agent.md` 缺口 9） |

> **模型切换选择器**：放在发送按钮旁边（`AiChatComposer` 的 `trailingAction` 槽位，`ModelSelector.tsx`）。目录与切换都走 Rust 侧（`agent_list_models` / `agent_set_model`），前端不持有模型状态——与 TUI 的 `/model` 同语义，只是从「浮层面板」变成「发送栏内联下拉」。

> **注意**：`AgentEvent` 目前是 `Debug`，**未派生 `Serialize`**。桥接层需要一个 `to_wire` 把 `AgentEvent` / `Message` 映射成前端 DTO（不直接给 SDK 加 `Serialize`，保持 SDK 契约小、协议差异收敛在边界——与 `docs/README.md` 原则一一致）。已落地为 `src/interface/desktop/wire.rs`。
>
> 为支撑工具卡，`ToolStarted` / `ToolFinished` 两个既有变体**扩展了字段**（`arguments` / `ok` / `output` / `elapsed_ms`）。这是 SDK 对外契约的一次小改：字段是**追加**的，既有消费者（TUI 用 `..` 忽略）零改动；仍未派生 `Serialize`。

---

**七、任务拆分（里程碑）**

**M0 · 骨架与文档**（已完成）
- [x] 本文档 `docs/desktop-interface.md`
- [x] 同步 `docs/README.md` / `Agent.md` 索引
- [x] 确认第四节剥离清单（审批保留代码、第一期不展示；Node 构建链已允许）

**M1 · 最小迁移：能渲染静态假数据**（已完成）
- [x] `main.rs` 抽 `Bootstrap` + `Mode` 分派（TUI 行为不变）
- [x] 建 `src/interface/desktop/`（Tauri 2 壳）+ `src/interface/desktop/web/`（React+TS+Vite+Tailwind 4）
- [x] copy 4.1 的组件与 `tokens.css`，去掉 shiwen import（`@shiwen/ui` → 本地）
- [x] 删 4.2 的定制逻辑（审批链路代码保留，渲染第一期不展示）
- [x] 前端 `npm run dev` 用 mock 桥渲染假对话（不依赖 Tauri 壳）

**M2 · 接入 SDK：发消息 → 流式回复闭环**（已完成）
- [x] `to_wire(AgentEvent) -> DTO`（`src/interface/desktop/wire.rs`）
- [x] Tauri command `agent_send` + event `agent://event`（`src/interface/desktop/shell.rs`）
- [x] 前端 `lib/bridge.ts` 消费事件（Tauri / mock 双模式），替换 shiwen 的 SSE 消费
- [x] `ToolStarted` / `ToolFinished` 事件扩展携带 `arguments` / `ok` / `output` / `elapsed_ms`

**M3 · 补齐会话与指令**
- [x] 模型切换选择器（放在发送按钮旁，`ModelSelector.tsx` + `agent_list_models` / `agent_set_model`）
- [x] `@` 引用：工作区文件/目录的提及浮层 + chip 展示（见 4.3；`agent_search_files` + `file-mentions/`）
- [ ] 会话列表 / 切换（`SessionCatalog`）
- [ ] `/login` `/session` 等指令的 desktop 呈现
- [ ] 思考显示 / 工具参数展开 / 滚动等交互对齐 TUI

**M4 · 收敛与文档**
- [x] markdown 渲染对齐 shiwen（补 `@source` 让 Tailwind 扫 streamdown，见 4.3.1）
- [ ] 若指令逻辑重复，下沉共享 `commands` 模块
- [ ] 更新 `Agent.md`、`docs/architecture.md`（新增 desktop 数据流图）

---

**八、验收口径**

1. `cargo run`（默认）行为与今天完全一致（TUI 不受影响）。
2. `cargo desktop`（= `cargo run --features desktop -- --desktop`）打开 Tauri 窗口，能发消息、看到流式回复与工具执行卡片。
3. 前端不出现任何 shiwen 专有概念（A2UI / entity / citation / space / share）；审批链路代码保留但第一期不渲染审批态。`@` 引用是**重绑到工作区文件**后的通用交互，不算 shiwen 概念。
4. `Agent` / `AgentEvent` 契约不变（SDK 未因桌面界面新增对外类型）。
5. `cargo test` 全绿（含既有红测试基线不变）。

---

**九、风险与未决**

- ~~**Node 工具链**：引入 npm 会改变仓库性质，需用户明确接受（已问）。~~ → **已确认允许**：用户接受引入 Node/npm/Vite 构建链。
- **`streamdown` 体量**：需要评估打包体积与离线可用性。
- **中文字体**：`tokens.css` 依赖 Songti / PingFang 等系统字体，跨平台需兜底。
- **窗口与 TUI 的会话一致性**：两个界面共享会话目录，需确认「同一时刻只开一个界面」还是允许并存（默认：启动时二选一，不同时开）。
