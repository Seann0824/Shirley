# 语音输入（Voice Input）

输入框上的麦克风按钮：说一句话，文字落到光标处。本文只讲**现在的实现**，历史上那套
「三引擎 + 云端上传」已经删除，删掉的原因写在 §7。

一句话原则：**录音和转写都发生在用户自己的设备上，音频永远不上服务器。**

## 1. 为什么是设备侧

同一个 Shirley 有两种运行时（桌面客户端 / 云端部署），语音的性质决定了它不能跟着运行时走：

| 用户形态 | 麦克风在哪 | 转写在哪 |
| --- | --- | --- |
| 桌面客户端（Tauri） | 用户电脑 | 用户电脑（本机 whisper.cpp）或 webview 自带识别 |
| 手机/浏览器打开 cloud | 用户的手机 | 用户的手机（浏览器 `SpeechRecognition`） |

手机打开云端项目时，服务器的 CPU 与用户的手机之间隔着一整条网络：把音频传过去，既多一次隐私
暴露（用户的语音落到了别人的机器上），又多一次失败点（上传、超时、格式）。而用户设备本来就带着
一个可用的识别器，没有任何理由绕路。

所以云端运行时**没有**任何语音路由：`interface/server/mod.rs` 里不存在 `agent_transcribe`，
`agent_asr_capabilities` 也一并删除。前端的 HTTP 桥对这两个方法直接返回「本机不做」。

## 2. 两条路径，一个按钮

| 路径 | 谁在识别 | 什么时候用 |
| --- | --- | --- |
| `web` | 浏览器/webview 自带的 `webkitSpeechRecognition` | 真浏览器（Chrome）：免费、有流式中间结果、不占 CPU |
| `local` | 本机 `whisper.cpp` CLI（Tauri 命令调用） | 桌面壳（Tauri/WKWebView）首选；`web` 不存在时兜底 |

选择是自动的：`useVoiceInput` 先看 `asrCapabilities()` 说本机有没有 `whisper.cpp`，再看浏览器
有没有识别器，然后取第一个能用的。**界面上没有选择器** —— 选引擎是实现问题，不是用户的决定；
给一个 `▾` 菜单就是把一次点击变成一道配置题。

### 2.1 桌面壳里 `local` 必须排在 `web` 前面

WKWebView（macOS 上的 Tauri 窗口）**暴露** `webkitSpeechRecognition`，但背后没有听写服务。
这条路径的失败方式是**沉默**：`start()` 不抛异常、`onerror` 不触发、`onend` 也不触发，于是按钮
一直红着闪，用户对着麦克风说话，什么都不会发生，也永远不会出现错误提示 —— 用错误处理器抓不到
它。两个后果：

- **顺序**：`runningInDesktopShell()` 为真时，候选顺序是 `local` 优先，`web` 只作后备。
- **看门狗**：即使排到了 `web`，8 秒内连 `audiostart` 都没有（正常识别器几百毫秒内必然触发），
  就判定为死服务，中止它并自动切到本机 whisper 接着录（`WEB_SILENCE_MS`）。
- 探测未返回（`probed === false`）时桌面壳**不放候选**：`capabilities === null` 时「本机没有
  whisper」和「还没问完」长得一模一样，等一个来回胜过打开一个没人听的麦克风。

真浏览器里规则不变：`web` 优先进（流式、省 CPU）。`web` 在 `service-not-allowed` / `network` /
`language-not-supported` 上失败时，如果本机有模型就静默换到 `local`，不打扰用户。

`Speaker` 之外的一条硬约束：云端/远程模式下，前端**不接受** `local` 之外的任何上传路径，
`http` 桥的 `transcribe` 直接抛错。

## 3. 音频格式：页面里就做成 16 kHz WAV

`web/src/lib/voice.ts` 用 `AudioContext` 采集，重采样到 16 kHz 单声道 16-bit，编码成 WAV 后
base64。这样 Rust 侧不需要 `ffmpeg`、不需要任何音频解码依赖（`whisper.cpp` 自己读 WAV）。
单次上限 `MAX_UTTERANCE_MS = 60_000`，没有 VAD：靠再点一次按钮或到点自动结束。

### 3.1 麦克风只申请一次

`getUserMedia()` 才是唤起权限弹窗的那次调用，而它原来每次按住都跑一遍：浏览器记得授权，但一次全
新的采集会话在 iOS Safari 和「每次按下都换一个音频会话」的 webview 里会把弹窗再举起来一次。现在
`voice.ts` 把流留温：

- `acquireMicStream()`：没有可用流才调 `getUserMedia`，否则复用同一个 `MediaStream`；
- `releaseMicSoon()`：说完话后 60s 释放（`stop()` 所有 track），OS 的麦克风指示灯不会一直亮着；
- `releaseMicNow()`：卸载、权限被吊销、引擎挂掉时立刻释放；
- `ensureMicPermission()`：只为拿到「这个 origin 允许用麦克风」这件事，拿到就立刻释放——浏览器
  `SpeechRecognition` 自己开采集，两路同时抓同一个设备会互相抢。

`web` 路径因此在识别器 `start()` **之前**先 `await ensureMicPermission()`：授权归 origin，识别器继承它，
弹窗整个浏览器只有一次。

## 4. 配置

```toml
[asr]
# 本机 whisper.cpp（只要 PATH 里有 whisper-cli 就自动启用）
local_binary = "/opt/homebrew/bin/whisper-cli"   # 可选，默认自动查找
local_model  = "/Users/me/.shirley/asr/models/ggml-base-q5_1.bin"  # 可选
local_language = "zh"                            # 可选，默认 auto
local_threads = 6                                # 可选
model_url = "https://hf-mirror.com/ggerganov/whisper.cpp/resolve/main/ggml-base-q5_1.bin"  # 可选
```

环境变量：`SHIRLEY_ASR_LOCAL_BINARY` / `SHIRLEY_ASR_LOCAL_MODEL` / `SHIRLEY_ASR_LOCAL_LANGUAGE` /
`SHIRLEY_ASR_LOCAL_THREADS` / `SHIRLEY_ASR_MODEL_URL`。

**Shirley 自带给的是模型，不是运行器。** 权重是用户自己搞不出来的那部分，所以第一次dictation时
自动下载 `ggml-base-q5_1.bin`（57 MiB，多语）到 `<global>/asr/models`（先写 `.part` 再 rename，
避免半截文件冒充可用模型）；运行器借机器的（`brew install whisper-cpp`）。内嵌 `whisper-rs`
试过并放弃：它要求构建机上有 `cmake`，这会让每个构建 desktop feature 的人都多一道门槛。

二进制查找不能只看 `PATH`：Tauri 从 Finder 启动时不继承交互 shell 的 `PATH`，于是额外扫
`/opt/homebrew/bin`、`/usr/local/bin`、`/usr/bin`、`/bin`、`~/.local/bin` 和 `<global>/asr/bin`。
模型同理：`<global>/asr/models`、`<global>/asr`、brew 的 `share/whisper-cpp/models`、
`~/.cache/whisper`，并用 1 MiB 下限过滤碎片文件。

## 5. 界面

输入框底部只有两组东西，和主流 composer 一致：

```
[ + ]  [来源]                                   [ 模型 ⌄ ] [ 🎤 ] [ ↑ ]
```

左侧一个 `+` 装下所有「往里面加东西」的动作（`ComposerAddMenu`：图片/文件、`@` 引用项目内文件、任务列表），
右侧是「这一轮怎么发出去」（`ModelSelector` + `VoiceInputButton` + 发送）。麦克风因此落在 `trailingAction`、
紧挨发送键，而不是和附件挤在左边 —— 早先它是 `leadingAction` 里的第四个图标，和 Attach 长得像、语义却无关。

`VoiceInputButton` 只有一个 `Button`。状态四种：空闲（麦克风图标）、启动麦克风（转圈，见 §3.1）、
录音中（ink 实心脉冲）、转写中（转圈，禁用点击）。没有 `available` 时整个按钮不渲染 —— 显示一个
按不动的按钮比不显示更糟。

**手机端这一个按钮不渲染。** 手机没有"语音模式"这个开关：输入条自己就是麦克风入口（见 5.1），
再多一个按钮只会多一个按错的地方。桌面端不受影响。

转写结果以**纯文本追加**到输入框末尾，不自动发送、不变成 chip：用户还要接着改（手机端
「按住说话」是这条规则的唯一例外，见 5.1 —— 松手本身就是发送动作）。

### 5.1 手机端：一行输入条，轻点打字 / 按住说话

767px 及以下只画**一行**：`[＋] [输入条] [模型] [↑]`（`AiChatComposer` 的 `layout="row"`）。输入条
（`VoiceHoldBar`）平时是一条「按住 转文字」的药丸，**它自己区分轻点和按住**，没有第二处模式开关：

| 手势 | 结果 |
| --- | --- |
| 轻点（< 220ms，`HOLD_TAP_MS`） | 同一个槽位换成 `MentionEditor`，键盘弹出，草稿原样在里面；**这一次已经开出去的麦被收掉**，不投递任何字 |
| 按住（≥ 220ms 且没滑走） | 抬全屏浮层（见下表）；麦克风在 `pointerdown` 时就已打开 |
| 手指滑出 `HOLD_TAP_SLOP_PX = 10px` | 收掉麦克风，也不弹键盘 —— 那是一次滚动或选字，不是我们的手势 |

判定是纯函数 `pressIntent(heldMs, movedPx)`（`web/src/lib/voice-hold.ts`），返回
`tap` / `hold` / `abandoned`；`VoiceHoldBar` 只负责拿定时器和指针位移去喂它。

**麦克风必须在 `pointerdown` 里打开**（`VoiceHoldBar` 的 `onPress`，同步调用 `voice.begin()`）。
这不是手感取舍，是浏览器的硬约束：Safari 只允许在用户手势内调用 `SpeechRecognition.start()`，
放在 `setTimeout` 或任何更晚的任务里，它会**照常开音频会话（系统录音指示灯亮）、然后一个字都不回**。
把开麦推迟到 220ms 定时器到点，换来的正是「第一次能用、之后只录音不出字」——第一次那声能用，
是因为权限弹窗上用户按的那一下「允许」把手势又续上了。代价是轻点也会短暂开一次麦（随即被收掉），
这比"按住说不出话"小得多。`onHold` 才是抬浮层那一半：麦克风归手势，浮层归确认。

键盘收起（失焦）就把药丸换回来，所以"回去说话"就是把键盘关掉，不需要切模式；草稿留在药丸里，
按住是继续追加、轻点是接着改。桌面端不受影响，仍是上下两层的 `MentionEditor` + 麦克风按钮。

按住之后的流程：

| 手势 | 结果 |
| --- | --- |
| 按住满 220ms | 先 `blur()` 收掉系统键盘（`pointerdown` 时已经做过一次），再抬起全屏浮层 `VoiceHoldOverlay`：中间一个 accent 气泡回显听到的字；`starting` 时写「正在打开麦克风…」，出声之后才写「正在听…」 |
| 松手（手指还在输入条高度） | `commit("send")`：把这一轮直接发出去；当前会话有回合在跑时**进 pending 队列**（走 `send()` → `submit()` 的同一条路，`busy` 就 `enqueueTurn`），浮层不立刻消失，留在 `transcribing` 上显示「转写中…」 |
| 按住后**把手指抬到线以上** | 松手 `cancel()`，一个字都不投递。不用瞄准 —— 拇指盖住屏幕时这是唯一好做的动作 |
| 按住后**抬到线以上、又落在右上角「编辑」小按钮上** | 松手 `commit("edit")`：文字留在输入框草稿里，**仍然停在语音模式**，按住条那行直接显示草稿，要改就自己点一下键盘切换 |
| 全程 | 跨过线时 `vibrate(8)`、松手 `vibrate(12)`，提示跟着变成「松手 取消 / 编辑 / 发送」，只有一根手指 |

没有最短按住时间：按下和松开就是整个手势，快也是「说完了」。什么都没听到的按压不产生文本，
也就不会变成空轮次。

**判定是「抬过一条线」，不是「压在哪个按钮上」**（`web/src/lib/voice-hold.ts`）：`zoneAt` 只看
`y < liftLineOf(origin.y)`（即起点上方 `HOLD_LIFT_PX = 72px`）。抬过线就默认取消，只有落进「编辑」
按钮 ±`HOLD_TARGET_SLOP_PX = 20px` 才改成编辑。上一版反过来，要求手指精确落在两个半屏大按钮上：
触点是指尖而不是按钮中心，而按钮就画在输入条高度，于是唯一能命中它们的方式是**在同一高度上横着滑**，
永远不是「往上拖」—— 规则和用户想要的（拖到上面才取消）对不上。现在两个目标缩成靠两侧的小药丸，
取消靠左、编辑靠右，因为取消根本不需要命中，它只需要被看见。

为什么松手就发：转写结果追加进输入框是「听写」（desktop 的语义），而按住说话是**把一句话说成一轮**。
两者是不同手势，不是同一个开关的两种状态，所以投递方式在松手时确定，由 `VoiceDelivery`
（`append` / `send` / `edit`）传给调用方，hook 自己不碰输入框。

**语音进行中不切到文字输入框。** `dictationHandler` 的 `edit` 分支原先会 `setVoiceComposer(false)` +
`editorRef.focus()`，于是拖到编辑这一下把键盘顶上来、语音条消失；用户的原话是「用语音的时候不要切到
输入框」。现在两种非发送投递都只写草稿，不聚焦、不切模式。唯一跨模式的动作是**用户自己轻点输入条**，
那是他要打字，不是程序替他决定。草稿要能看见，所以 `VoiceHoldBar` 的 `draft` 有内容时显示草稿而不是
「按住 转文字」。

浮层的两条硬要求（都是踩过的）：

- **浮层比手指活得久**：`App` 用 `holding || state === "transcribing"` 决定是否渲染浮层，松手只是把
  `armed` 置 false（浮层不再接管指针）。上一版松手即卸载，本机 whisper 转写那几秒屏幕上什么都没有，
  用户以为白说了。
- **错误必须画在浮层里**：授权被拒时 `voice.error` 原先只出现在 composer 的提示行上，而那行正被全屏
  浮层盖住 —— 用户面对一个按不动的遮罩，没有任何解释。现在错误进气泡、1.8s 后自动收起（`cancel`）。

### 5.2 状态机：为什么不是几个 `useState`

语音是这个应用里唯一同时被**手指、麦克风、识别器、转写器**推动的功能。按下、松手、识别器开完、
来了半句、转写回来、超时、授权被拒 —— 这些顺序任意交错，而其中只有一部分是用户的意图。第一版把状态
散在五个回调里（`listeningRef` / `settledRef` / `epochRef` / `state` / `watchdogRef`），于是每修一个
卡死就多一个 `if`，最后 `if` 之间存在互相漏气的组合：真机上表现为「只有第一次能语音输入」
（停在 `listening` 的识别器 + `begin()` 的 `state !== "idle"` 静默拦截）。

现在状态机是**独立模块** `web/src/lib/voice-machine.ts`：纯 reducer，没有 DOM、没有 React、没有浏览器
API，`voice-machine.test.ts` 用 13 个用例直接驱动它。规则只有三条：

- **每次按下都是新的一段（generation）**：`press` 把 `gen` 加一。所有异步回调回来时必须报出它属于哪个
  `gen`，对不上就整条丢弃 —— 「上一代迟到的文字」和「上一代迟到的失败」都不可能污染这一段。
- **按下永不静默**：`press` 先收掉残留（`abort`），再开新的。不存在「这次按下被忽略」这种结果，那正是
  用户按下没反应时的感受。
- **副作用由 reducer 声明，不由它执行**：reducer 只返回 `open` / `close` / `abort` / `deliver` 四个
  effect，谁来开麦、谁来交付由适配器决定。

`useVoiceInput` 因此退化成薄适配器：把浏览器/硬件事件翻成 `VoiceEvent`，执行 effects，并且在每次
异步回来后问一句 `alive(gen)`。它自己不再持有任何「现在到底在不在听」的判断 —— 那个判断只有一个地方。

`release` 的语义分引擎：浏览器引擎在 `listening` 时先交付再 `abort`（它的结果是累计的，
`readTranscript` 每帧重读整份列表，天然幂等）；本机引擎要等音频真的转写完，所以先进 `finishing`，
`settled` 到了才交付，中途再按一下就让旧结果作废。60 秒上限（`MAX_UTTERANCE_MS`）等价于松手。

错误只给一句人话（Sean 的长期要求：不暴露状态码和技术细节）：

| 文案 | 场景 |
| --- | --- |
| `Voice input failed; try again.` | CLI 失败，或跑到 usage 输出 |
| `Voice input heard nothing; try again.` | 成功退出但没有文本 |
| `Voice input took too long; try a shorter recording.` | 超过 180 s |
| `The recording was empty; try again.` | 音频为空/不是 WAV |
| `Microphone permission denied — allow it in System Settings › Privacy & Security › Microphone.` | `NotAllowedError` / `SecurityError` |
| `This window cannot reach the microphone.` | `TypeError`（没有 secure context） |

失败证据写到 `<global>/asr/last-command.log`：二进制路径、完整 argv、退出状态、CLI 输出末
4000 字符、音频字节数与语言。**这条日志是排障的唯一入口** —— UI 只有一句话，细节全在这里。

### 5.3 识别器生命周期：一个实例，复用到底

状态机管的是「这一句话」；识别器管的是「音频会话」，而音频会话比一次 JavaScript 调用活得久得多。

真机上报的症状是：「发完第一条消息后，要等一会才能继续语音输入」。不是永久失效，是等几秒自己好。
根因不在状态机，而在识别器怎么被创建：

- **旧写法每次按下都 `new` 一个新识别器**。`abort()` 是立即返回的，但浏览器的音频会话还在收尾；
  在这个窗口里对**第二个实例**调 `start()`，Safari 要么直接拒绝，要么更糟 —— 接受：麦克风指示灯亮着，
  却一个字都不回。等旧会话终于 `end` 了，一切又好了。这就是「等一会」。
- 而**发完一条消息**恰恰是最容易撞上这个窗口的时刻：交付 → `abort` → 用户立刻又按下去。

修法不是再加一个 `if`，而是把生命周期本身收成一个对象 `web/src/lib/recognizer.ts` 的
`RecognizerSlot`（纯记账，没有 DOM、没有定时器，12 个用例在 `recognizer.test.ts`）：

| 规则 | 为什么 |
| --- | --- |
| **一个 handle 复用到底**，只在首按创建一次 | 没有第二个实例，就没有两个音频会话互相抢 |
| `start()` 遇到还开着的会话 → **不硬起**，`stop()` 并记 `owed` | 这才是浏览器唯一接受的顺序：先让它结束 |
| `ended()` 偿还 `owed`，返回 `true` | 等待变成确定的一两百毫秒，而不是听天由命 |
| `stop()` 是优雅结束 | 只有「放弃这一句、但还愿意等」时才用它；交付完成后不用（见 §5.4） |
| `abort()` 之后**仍然算 open**，直到 `end` 到达 | 假装它已经关了，正是这个 bug 的来源 |
| `reset()` 用于放弃会话（失败后） | 死会话不能永远挡住下一次按下 |
| `stop()` 同时清掉 `owed` | 手指在等会话关闭时已经抬起，就不能再被 `end` 重新拉起 |

适配器侧（`useVoiceInput.ts`）因此只剩三件事：handlers **只绑一次**并读 `genRef`（识别器比任何一代都活得久，
捕获 `gen` 会立刻过期）；`start()` 的返回值决定「已开 / 排队 / 被拒」；`onSessionEnd` 分两路 —— 有 `owed`
就接着开，没有就看是「已结束的尾巴」还是「Chrome 自己结束了但手指还按着」。

失败与正常收尾也在这里分开：`emit` 把 `event.type === "failed"` 作为「硬」标记透传给 `apply`，
失败走 `reset()`（下一按能立刻重开），其余 `abort` effect 走 `abort()`（会话立刻开始收尾）。

### 5.4 一次按住 = 一次会话，松手就是交付

§5.3 解决了「两个实例抢一个音频会话」，但真机上还有第二个症状：**快速按第二次会报「麦克风没有响应」，
而且一句话明明说得清清楚楚却被判成「没听清」**。两个原因，都在同一条错误的思路里 ——
把「用户说完一句话」交给浏览器去宣布。

- **等 `end` 才交付，等于把交付交给平台。** 旧流程是：松手 → `stop()` → 等浏览器回 `onend` →
  用那时的最终结果交付。iOS 上这个 `onend` 可以晚到几秒，而用户已经又按下去了；
  那一下正好落在旧会话的关闭窗口里。
- **`onend` 之后拿到的那份「最终结果」常常是空的。** WebKit 不支持 `continuous`，
  按住说话时经常**只产出临时结果（interim）**。旧代码把 `settled`（final）当唯一事实，
  临时结果只拿来显示 —— 于是屏幕上一直有字，松手却被判成「没听清，再说一次」。
  这是「录上了但没转写」最隐蔽的一条。

因此契约改成一句话：**屏幕上看到的字，就是这一句。** 具体三条：

| 规则 | 为什么 |
| --- | --- |
| 松手立刻交付 `settled + interim`，然后 `abort()` | 交付不再依赖任何回调；迟到的最终结果本来也不会更好看 |
| 不再发 `stop()`（`web-ended` 事件随之删除） | 请求它「优雅结束」只是把等待变成礼貌的等待 |
| `start()` 遇到还开着的会话：`abort()` + **当场重开** | 上一句已经交付，没什么可收集的；`queued` 只留给「连 abort 后重开都被拒」的兜底 |

代价写在明处：`abort()` 会丢掉最后几百毫秒里尚未成文的那点尾音。用「已定稿 + 临时结果」合并来补，
最差是最后一个字略有偏差 —— 换的是永不卡住、永不把说过的话丢掉。

对应测试：`voice-machine.test.ts` 的「只有临时结果也照样交付」与「松手立刻交付并硬关会话，不等 `end`」；
`recognizer.test.ts` 的「按下时旧会话还开着 → abort 并当场重开」与「send 后立刻再按，第二句在录、不在排队」。

## 6. macOS 权限：为什么必须有 Info.plist

macOS 的 TCC 只认应用包里的 `NSMicrophoneUsageDescription`。没有它，`getUserMedia` 直接抛
`NotAllowedError`，用户看到的是「权限被拒」而不是一个能点的授权弹窗。

- `crates/shirley/Info.plist` 声明 `NSMicrophoneUsageDescription`
- `crates/shirley/Entitlements.plist` 声明 `com.apple.security.device.audio-input`
- `crates/shirley/build.rs` 在 macOS + `desktop` feature 下发 `-Wl,-sectcreate,__TEXT,__info_plist,<abs>`
  （`cargo run` 跑的是裸二进制，没有 bundle，只能这样嵌进去；用 `otool -s __TEXT __info_plist` 验证）
- `tauri.conf.json` 的 `bundle.macOS` 指向 entitlements，`minimumSystemVersion = "12.0"`

注意：`getUserMedia` 在 macOS WKWebView 里由 WebKit 的媒体进程承接，实测**不弹窗也能录音**，
所以「日志里已经有 WAV」不等于「TCC 已经放行」——两者是不同环节。

### 6.1 怎么确认权限这一环是否已经通了

「语音用不了」最常见的猜测是「没申请 macOS 麦克风权限」。这个猜测可以用日志直接证伪，不必凭感觉
改权限代码：

```sh
otool -s __TEXT __info_plist target/debug/Shirley | head        # plist 是否嵌进裸二进制
log show --last 3h --predicate 'subsystem == "com.apple.TCC"' \
  | grep kTCCServiceMicrophone                                  # 有没有 request，authValue 是多少
```

判读方式：出现 `service=kTCCServiceMicrophone ... preflight=yes query=1` 且随后
`AUTHREQ_RESULT ... authValue=2` 就是**已授权**（`authValue=0` 才是被拒）；进程没有被 TCC
直接杀掉，也说明 plist 是有效的。真到了这一步还不行，问题一定在权限下游（引擎选择、录音、
转写），别再回头动 plist。实际排障记录见 §10。

## 7. 语言码只传基础码

浏览器识别器的 `lang` **不看** `navigator.language`：系统的区域设置描述的是这台手机，不是用户
正在说的那句话。英文系统的手机说中文，`lang='en-US'` 就会把中文听成英文单词再原样返回英文。
`preferredLanguage()` 因此返回 `zh-CN`（这个界面说的是中文），要改的人写
`localStorage["shirley.voice.language"]`，不需要多一个设置界面。

`navigator.language` 是 BCP-47（`en-US`、`zh-CN`），而 `whisper.cpp` 只认基础码。踩过的坑：
把 `en-US` 直接塞给 `-l`，CLI 打印 `whisper_lang_id: unknown language 'en-us'`，随即输出整页
usage，**退出码却是 0**，`-otxt` 写出的 `.txt` 是 0 字节 —— 前端只看到「失败」，日志之外毫无线索。

因此 `whisper_language()` 一律剥掉区域子标签（`en-US` → `en`、`zh_CN` → `zh`、`pt_BR` → `pt`、
空/`auto` → `auto`）；`whisper_args()` 集中构造 argv，重试时参数不漂移；输出里出现
`unknown language` 时自动用 `-l auto` 重跑一次 —— 录音不该被语言猜测丢掉。

另外两条来自同一个坑：音频作为**位置参数**传入（没有输入文件时 CLI 会打印 usage 并成功退出），
转写只读 `-otxt -of <stem>` 写出的 `.txt`，**绝不从 stdout 读文本**（stdout 和 CLI 自己的日志混在
一起）。

## 8. 删除云端引擎的理由

历史实现有三个引擎，优先级 `web` → `local` → `cloud`，`cloud` 走 OpenAI 兼容的
`/audio/transcriptions`，base_url / key 回退到当前模型供应商。它被删掉，有三个各自独立的理由：

1. **隐私与架构**：音频离开设备本身就违背「转写在用户设备上」这一条，而这条现在是硬约束。
2. **多数供应商不实现**：DeepSeek 等 OpenAI 兼容服务根本没有 `/audio/transcriptions`，把「别人
   没实现的功能」当成默认路径，等于把第三方缺口变成用户的死路。
3. **它是多出来的失败面**：上传、超时、mime、multipart 全是额外可能出错的环节，收益却是零。

现在 `AsrCapabilities` 只描述本机：`local_available` / `local_binary` / `local_model` /
`local_model_ready` / `hint`。`hint` 在缺 `whisper.cpp` 时直接给出一行修法
（`brew install whisper-cpp`），缺模型时说明会在第一次使用时下载一次。

## 9. 已知边界

- 没有 VAD / 静音检测：靠第二次点击或 60 s 上限结束
- 手机 Safari 上 `webkitSpeechRecognition` 的可用性与行为未实测（不可用时应回落到「没有本地模型
  → 按钮不显示」，即手机上普遍靠浏览器识别）
- Tauri webview 的语音路径未端到端手动点测过
- 云端运行时完全不参与语音：这是设计，不是缺失

## 10. 一次真实的排障记录（2026-10，「tauri 端还是用不了」）

现象：桌面端点麦克风没有任何效果，用户怀疑是没申请 macOS 麦克风权限。按顺序查，前两个假设都被
证据否掉了：

| 假设 | 证据 | 结论 |
| --- | --- | --- |
| 没申请麦克风权限 | TCC 日志有 `kTCCServiceMicrophone` 的 `preflight=yes query=1`，且 `AUTHREQ_RESULT authValue=2`（允许）；`otool -s __TEXT __info_plist` 有内容 | **否**，权限已授予且 plist 已嵌入 |
| whisper-cli 打印 usage 是参数拼错了 | `~/.shirley/asr/last-command.log` 里的 argv 是 `-l en-US`（whisper.cpp 只认基础码，非法语言会打印用法并以退出码 0 结束）；当前代码已有 `whisper_language()` 剥离子标签，且 `asr.rs` 的 mtime 早于二进制 | 旧二进制的问题，已修 |
| **引擎选到了死掉的 `web` 识别器** | WKWebView 暴露 `webkitSpeechRecognition` 但无听写服务，且失败时**不报错** | **是**，本次修复 |

另外记一条容易踩的坑：`dist` 是**编译期嵌进二进制**的（`tauri.conf.json` 的 `frontendDist`），
只改前端不重新 `cargo build --features desktop`，跑起来的还是旧界面 —— 这本身就足以让人误判
「改了没用」。
