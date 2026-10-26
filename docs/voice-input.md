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

`VoiceInputButton` 只有一个 `Button`。状态三种：空闲（麦克风图标）、录音中（红色脉冲）、转写中（转圈，禁用点击）。
没有 `available` 时整个按钮不渲染 —— 显示一个按不动的按钮比不显示更糟。

转写结果以**纯文本追加**到输入框末尾，不自动发送、不变成 chip：用户还要接着改。

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
