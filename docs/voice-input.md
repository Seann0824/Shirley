# Voice input

Hold the button and speak; text appears while you talk. On release the final transcript is
sent; slide up before releasing to put it in the text field and edit it first.

## How it works

The browser records 16 kHz mono WAV (`web/src/components/recorder.ts`) and the server
transcribes it with the `whisper.cpp` CLI (`crates/shirley/src/infra/asr.rs`).

- **While you hold**: about once a second the page posts the recording so far to
  `POST /api/transcribe?partial=1` and shows the result. The interval grows with the recording
  (each preview re-transcribes everything said so far). A preview is skipped, not queued, when
  the server is busy (at most two whisper processes at once).
- **On release**: one final `POST /api/transcribe`, decoded the same way as the previews
  (greedy), so it only adds what was said after the last preview instead of re-deciding words
  the user already saw. (Beam search changed earlier words and bought nothing measurable.)
- **Language**: the first of `[asr] languages` (default `zh`, then `en`) is the user's own.
  Whisper detects the language, but anything else wins only when it is allowed and whisper is
  sure (probability ≥ 0.8); otherwise the audio is transcribed as the user's own language. Its
  guess from a second or two of speech is unreliable -- short Chinese came back as English or
  Japanese, and a wrong language kept for the utterance turned every later preview into
  nonsense in that language, which looked like correct text suddenly changing. The result is
  kept for the utterance once there are 1.5 s of speech. `[asr] local_language` pins one
  language instead.
- **Prompt echo**: whisper sometimes repeats a clause of its prompt ("使用简体中文。") at the start
  or end of a transcript; those are stripped.
- **Gesture**: release in place and the text is sent; slide up before releasing and it goes
  into the text field to check and edit.
- **No whisper.cpp on the server**: the page falls back to the browser's own recogniser, fixed to
  Chinese (browsers cannot detect the language).

**Model**: `ggml-small-q5_1` (190 MB), downloaded at server start; an older `base` model keeps
serving until it has arrived. On Chinese, `base` gets whole words wrong and changes them from
pass to pass; `small` is far closer and steadier, at about twice the work. Measured on a 4-core
x86 server: a preview or the final pass of a 3 s utterance takes 1.5-2 s with `small` (about
0.6 s with `base`). On a slow server set `[asr] local_model` to a `ggml-base-q5_1.bin`.

## Deploying on CentOS 7

### 1. HTTPS is required

Browsers only let **https** pages (or `localhost`) use the microphone. Over plain http the button
reports "浏览器只允许 HTTPS 页面使用麦克风". Put Shirley behind nginx with a certificate:

```nginx
server {
    listen 443 ssl http2;
    server_name shirley.example.com;
    ssl_certificate     /etc/nginx/ssl/fullchain.pem;
    ssl_certificate_key /etc/nginx/ssl/privkey.pem;

    client_max_body_size 64m;          # uploads and recordings

    location /api/events {             # server-sent events: no buffering
        proxy_pass http://127.0.0.1:8787;
        proxy_http_version 1.1;
        proxy_set_header Connection "";
        proxy_buffering off;
        proxy_read_timeout 1h;
    }
    location / {
        proxy_pass http://127.0.0.1:8787;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
    }
}
```

Set `SHIRLEY_SERVER_TOKEN` and open the page with `?token=...` when it is reachable from the
internet.

### 2. Build whisper.cpp

CentOS 7's gcc 4.8 cannot build whisper.cpp (it needs C++17). Use a newer toolchain from SCL
(CentOS 7 is end-of-life, so point the repos at `vault.centos.org` first if `yum` cannot find
them) and `cmake3` from EPEL:

```sh
yum install -y epel-release centos-release-scl
yum install -y devtoolset-11-gcc-c++ cmake3 git
scl enable devtoolset-11 bash

git clone --depth 1 https://github.com/ggerganov/whisper.cpp /opt/whisper.cpp
cd /opt/whisper.cpp
cmake3 -B build -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF -DWHISPER_BUILD_TESTS=OFF
cmake3 --build build -j"$(nproc)" --target whisper-cli
```

Shirley finds `/opt/whisper.cpp/build/bin/whisper-cli` on its own. Elsewhere, set
`[asr] local_binary` or `SHIRLEY_ASR_LOCAL_BINARY`.

### 3. The model

Downloaded automatically at server start (190 MB, `hf-mirror.com` first, then Hugging Face) into
`~/.shirley/asr/models/`. Without internet access, copy `ggml-small-q5_1.bin` there by hand or
set `[asr] local_model`.

### 4. Check

```sh
curl -s -X POST http://127.0.0.1:8787/api/cmd/agent_asr_status
# {"available":true,"model_ready":true,"hint":""}
```

The last failed whisper invocation (command line and output) is kept in
`~/.shirley/asr/last-command.log`.

## Configuration

```toml
[asr]
local_binary   = "/opt/whisper.cpp/build/bin/whisper-cli"   # default: auto-detected
local_model    = "/data/models/ggml-small-q5_1.bin"         # default: managed download
model_url      = "https://example.com/ggml-small-q5_1.bin"  # where to download it from
local_language = "zh"        # pin the language; default: detect per utterance
languages      = ["zh", "en"] # yours first; another wins only when whisper is sure (p >= 0.8)
local_threads  = 4           # default: cores, at most 8
prompt         = ""          # default nudges Chinese to simplified characters; "" sends none
```

Environment overrides: `SHIRLEY_ASR_LOCAL_BINARY`, `SHIRLEY_ASR_LOCAL_MODEL`,
`SHIRLEY_ASR_MODEL_URL`, `SHIRLEY_ASR_LANGUAGE`.
