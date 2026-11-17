//! Voice input: speech to text on the server, with `whisper.cpp`.
//!
//! The page records 16 kHz mono WAV and posts it to `/api/transcribe`; this module runs the
//! `whisper.cpp` CLI over it and returns the text. The server is the user's own (the same machine
//! already holds every message they send), so the audio does not reach anyone new.
//!
//! While the user holds the button the page re-sends the recording so far about once a second
//! ("partial" requests) and shows each result, which is how the text appears while they speak.
//! Partials are best-effort: when every transcription slot is busy one is skipped rather than
//! queued, so a long utterance cannot pile up work on a small server. The final transcription on
//! release always waits for a slot.
//!
//! The language is detected by whisper itself unless `[asr] local_language` pins one. Detection
//! costs a full extra encoder pass (about a second and a half on a 4-core server), so it happens
//! once per utterance: the first result reports the detected language and the page sends it back
//! with every later request for the same recording. Partials decode greedily (about half a
//! second); the final transcription uses beam search for accuracy.
//!
//! **Shirley ships the model, not the runner.** The multilingual `base` model (57 MiB) is
//! downloaded once into `<global>/asr/models`, in the background at startup; the CLI comes from
//! the machine (`brew install whisper-cpp`, or built from source on a server).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::{Mutex, Semaphore};

use super::settings::FileSettings;

/// One utterance cap. 24 MiB of 16 kHz mono PCM is roughly 12 minutes of speech.
pub const MAX_AUDIO_BYTES: usize = 24 * 1024 * 1024;
/// A partial shorter than this is not worth a process: whisper invents words for near-silence.
const MIN_PARTIAL_BYTES: usize = 44 + 16_000 * 2 / 2; // half a second
/// Local whisper on CPU is slow, but an unbounded child process is worse.
const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(180);
/// How many whisper processes may run at once. Each uses several cores; more than this only
/// makes all of them slower.
const MAX_CONCURRENT: usize = 2;
/// Local CLI candidates, tried in order when `local_binary` is not configured.
const LOCAL_BINARY_CANDIDATES: [&str; 3] = ["whisper-cli", "whisper-cpp", "whisper"];
/// Directories probed for the CLI in addition to `PATH`: a service manager or a GUI launch does
/// not inherit the interactive shell's `PATH`.
const BINARY_DIRS: [&str; 5] = [
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/opt/whisper.cpp/build/bin",
    "/usr/bin",
    "/bin",
];
/// Directories probed for an existing ggml model, in order, before downloading one.
const MODEL_DIRS: [&str; 3] = [
    "/opt/homebrew/share/whisper-cpp/models",
    "/usr/local/share/whisper-cpp/models",
    "/opt/whisper.cpp/models",
];
/// The model Shirley manages: `base` quantised to q5_1 is 57 MiB and multilingual. Smaller
/// (`tiny`) is too weak for Chinese; larger (`small`, 466 MiB) is too slow on a CPU server for
/// text that appears while you speak.
const DEFAULT_MODEL_FILE: &str = "ggml-base-q5_1.bin";
/// Where to fetch the managed model, tried in order. The mirror first: the Hugging Face host is
/// unreachable from mainland China.
const DEFAULT_MODEL_URLS: [&str; 2] = [
    "https://hf-mirror.com/ggerganov/whisper.cpp/resolve/main",
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main",
];
const MODEL_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// A truncated download must not be cached as a model.
const MIN_MODEL_BYTES: usize = 8 * 1024 * 1024;
/// Steers Chinese output to simplified characters (whisper often writes traditional ones). It
/// only biases wording; the language is still detected from the audio.
const DEFAULT_PROMPT: &str = "以下是普通话的句子，使用简体中文。";
/// Audio needed before a detected language is reported back and kept for the utterance.
const MIN_DETECT_SECONDS: f64 = 1.5;
/// Languages detection may settle on unless `[asr] languages` says otherwise.
const DEFAULT_LANGUAGES: [&str; 2] = ["zh", "en"];

/// What the server can do, computed on every call so installing whisper.cpp takes effect
/// without a restart.
#[derive(Debug, Clone, Serialize)]
pub struct AsrStatus {
    /// Whether this server can transcribe right now (the CLI is installed).
    pub available: bool,
    /// Whether the model is on disk; `false` means the first transcription waits for it.
    pub model_ready: bool,
    /// One short setup hint; empty when everything is ready.
    pub hint: String,
}

/// One transcription.
#[derive(Debug, Clone, Serialize)]
pub struct AsrResult {
    /// What the model heard; empty when it heard nothing.
    pub text: String,
    /// The language whisper used (`zh`, `en`, ...): detected on the first request of an
    /// utterance, to be sent back with the next ones.
    pub language: Option<String>,
    /// Wall-clock cost of the transcription.
    pub elapsed_ms: u64,
}

/// Why a transcription produced no text.
#[derive(Debug)]
pub enum AsrError {
    /// A partial was skipped because every slot was busy. Not a failure.
    Busy,
    /// A short message the composer can show.
    Failed(String),
}

/// Resolved ASR settings: the `[asr]` table with `SHIRLEY_ASR_*` environment overrides on top.
#[derive(Debug, Clone, Default)]
struct AsrConfig {
    local_binary: Option<String>,
    local_model: Option<String>,
    model_url: Option<String>,
    local_language: Option<String>,
    local_threads: Option<u32>,
    prompt: Option<String>,
    languages: Vec<String>,
}

impl AsrConfig {
    /// A missing or broken config file is not an error: voice input must never block startup.
    fn load(global: &Path) -> Self {
        let file = FileSettings::load_file(&global.join("config.toml"))
            .ok()
            .flatten()
            .map(|settings| settings.asr)
            .unwrap_or_default();
        let env = |key: &str| {
            std::env::var(key)
                .ok()
                .filter(|item| !item.trim().is_empty())
        };
        Self {
            local_binary: env("SHIRLEY_ASR_LOCAL_BINARY").or(file.local_binary),
            local_model: env("SHIRLEY_ASR_LOCAL_MODEL").or(file.local_model),
            model_url: env("SHIRLEY_ASR_MODEL_URL").or(file.model_url),
            local_language: env("SHIRLEY_ASR_LANGUAGE").or(file.local_language),
            local_threads: file.local_threads,
            prompt: file.prompt,
            languages: allowed_languages(file.languages),
        }
    }
}

/// Languages auto-detection may settle on: the configured list (plain codes only), else zh, en.
fn allowed_languages(configured: Option<Vec<String>>) -> Vec<String> {
    let list: Vec<String> = configured
        .unwrap_or_default()
        .iter()
        .map(|tag| whisper_language(Some(tag)))
        .filter(|code| code != "auto")
        .collect();
    if list.is_empty() {
        DEFAULT_LANGUAGES.iter().map(|&code| code.to_owned()).collect()
    } else {
        list
    }
}

/// What detection settled on, or the first allowed language when it heard something else.
/// `None` (nothing detected) is left alone.
fn constrain_language(detected: Option<String>, allowed: &[String]) -> Option<String> {
    let detected = detected?;
    if allowed.contains(&detected) {
        Some(detected)
    } else {
        allowed.first().cloned()
    }
}

fn slots() -> &'static Semaphore {
    static SLOTS: OnceLock<Semaphore> = OnceLock::new();
    SLOTS.get_or_init(|| Semaphore::new(MAX_CONCURRENT))
}

/// Serialises model downloads: two first requests must not fetch 57 MiB twice.
fn download_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Report what this server can transcribe with, and what is missing.
pub fn status(global: &Path) -> AsrStatus {
    let config = AsrConfig::load(global);
    let binary = resolve_local_binary(&config, global);
    let model = resolve_local_model(global, config.local_model.as_deref());
    let hint = if binary.is_none() {
        "服务器没有安装 whisper.cpp，语音改用浏览器识别（只认中文）。".to_owned()
    } else if model.is_none() {
        "语音模型正在下载（约 57 MB），完成后即可使用。".to_owned()
    } else {
        String::new()
    };
    AsrStatus {
        available: binary.is_some(),
        model_ready: model.is_some(),
        hint,
    }
}

/// Fetch the model in the background if whisper.cpp is installed and the model is missing, so
/// the first press of the button does not wait for a download.
pub fn prepare(global: PathBuf) {
    let config = AsrConfig::load(&global);
    if resolve_local_binary(&config, &global).is_none()
        || resolve_local_model(&global, config.local_model.as_deref()).is_some()
    {
        return;
    }
    tokio::spawn(async move {
        if let Err(error) = ensure_model(&global, &config).await {
            eprintln!("[asr] {error}");
        }
    });
}

/// Transcribe a WAV recording.
///
/// A `partial` request is the live preview while the user is still speaking: it gives up at once
/// with [`AsrError::Busy`] when no slot is free, and stays quiet about near-silent audio.
///
/// # Errors
/// [`AsrError::Busy`] for a skipped partial; otherwise a short user-facing message.
pub async fn transcribe(
    global: &Path,
    wav: &[u8],
    partial: bool,
    language: Option<&str>,
) -> Result<AsrResult, AsrError> {
    let started = Instant::now();
    if partial && wav.len() < MIN_PARTIAL_BYTES {
        return Ok(AsrResult {
            text: String::new(),
            language: language.map(str::to_owned),
            elapsed_ms: 0,
        });
    }
    require_wav(wav).map_err(AsrError::Failed)?;
    let _slot = if partial {
        slots().try_acquire().map_err(|_error| AsrError::Busy)?
    } else {
        slots()
            .acquire()
            .await
            .map_err(|_error| AsrError::Failed(GENERIC_FAILURE.to_owned()))?
    };
    let config = AsrConfig::load(global);
    // A pin in the config wins; then the language this utterance was already detected as.
    let language = config
        .local_language
        .as_deref()
        .or(language)
        .map_or_else(|| "auto".to_owned(), |tag| whisper_language(Some(tag)));
    let detecting = language == "auto";
    let (text, mut language) = transcribe_local(&config, global, wav, language, partial)
        .await
        .map_err(AsrError::Failed)?;
    // A guess from the first second of speech is not worth keeping for the whole utterance:
    // report it only once there is enough audio, so the next preview detects again.
    if detecting && wav_seconds(wav) < MIN_DETECT_SECONDS {
        language = None;
    }
    Ok(AsrResult {
        text,
        language,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

/// One short line for the composer when nothing more specific can be said. The technical
/// reason goes to `<global>/asr/last-command.log`.
const GENERIC_FAILURE: &str = "语音转写失败，请再试一次。";

/// Run the `whisper.cpp` CLI over a temp WAV file.
///
/// Two rules learned from real failures: the audio goes in as a **positional** argument (the
/// only form every release accepts; without an input file the CLI prints its usage and exits
/// 0), and the transcript is read from the `-otxt` file, never from stdout.
async fn transcribe_local(
    config: &AsrConfig,
    global: &Path,
    wav: &[u8],
    mut language: String,
    greedy: bool,
) -> Result<(String, Option<String>), String> {
    let binary = resolve_local_binary(config, global)
        .ok_or_else(|| "这台服务器没有安装语音转写（whisper.cpp）。".to_owned())?;
    let model = ensure_model(global, config).await?;

    let dir = global.join("asr").join("tmp");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|_error| GENERIC_FAILURE.to_owned())?;
    let stem = uuid::Uuid::new_v4().to_string();
    let audio_path = dir.join(format!("utterance-{stem}.wav"));
    // `-of` takes a path *without* extension: the CLI appends `.txt` itself.
    let output_stem = dir.join(format!("transcript-{stem}"));
    let transcript_path = output_stem.with_extension("txt");
    tokio::fs::write(&audio_path, wav)
        .await
        .map_err(|_error| GENERIC_FAILURE.to_owned())?;

    let threads = config.local_threads.unwrap_or_else(default_threads);
    let prompt = config.prompt.as_deref().unwrap_or(DEFAULT_PROMPT);
    let mut retried = false;
    let (args, output) = loop {
        let args = whisper_args(&WhisperRun {
            model: &model,
            output_stem: &output_stem,
            audio: &audio_path,
            language: &language,
            threads,
            prompt,
            audio_ctx: audio_context(wav_seconds(wav)),
            greedy,
        });
        let run = tokio::process::Command::new(&binary)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        let output = match tokio::time::timeout(TRANSCRIBE_TIMEOUT, run).await {
            Err(_) => {
                let _ = tokio::fs::remove_file(&audio_path).await;
                return Err("这段录音太长了，转写超时。".to_owned());
            }
            Ok(Err(error)) => {
                let _ = tokio::fs::remove_file(&audio_path).await;
                record_failure(global, &binary, &args, &format!("spawn failed: {error}")).await;
                return Err(GENERIC_FAILURE.to_owned());
            }
            Ok(Ok(output)) => output,
        };
        if output.status.success() {
            let written = tokio::fs::read_to_string(&transcript_path)
                .await
                .unwrap_or_default();
            let _ = tokio::fs::remove_file(&audio_path).await;
            let _ = tokio::fs::remove_file(&transcript_path).await;
            if !looks_like_usage(&String::from_utf8_lossy(&output.stdout)) {
                if language != "auto" {
                    return Ok((clean_transcript(&written, prompt), Some(language)));
                }
                let detected = detected_language(&String::from_utf8_lossy(&output.stderr));
                let used = constrain_language(detected.clone(), &config.languages);
                // Heard as a language the user does not speak: transcribe again as one they do.
                if used != detected && !retried {
                    if let Some(code) = used {
                        language = code;
                        retried = true;
                        tokio::fs::write(&audio_path, wav)
                            .await
                            .map_err(|_error| GENERIC_FAILURE.to_owned())?;
                        continue;
                    }
                }
                return Ok((clean_transcript(&written, prompt), used));
            }
        }
        let noise = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        // A pinned language this build does not know: `auto` always works.
        if !retried && language != "auto" && noise.contains("unknown language") {
            "auto".clone_into(&mut language);
            retried = true;
            continue;
        }
        break (args, output);
    };

    let _ = tokio::fs::remove_file(&audio_path).await;
    let _ = tokio::fs::remove_file(&transcript_path).await;
    let noise = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    record_failure(
        global,
        &binary,
        &args,
        &format!(
            "audio: {} bytes, language: {language}\nstatus: {}\n{noise}",
            wav.len(),
            output.status
        ),
    )
    .await;
    Err(GENERIC_FAILURE.to_owned())
}

/// `whisper.cpp` only knows base language codes (`en`, `zh`). A full tag (`zh-CN`) makes it
/// print its usage and exit 0.
fn whisper_language(tag: Option<&str>) -> String {
    let Some(tag) = tag.map(str::trim).filter(|tag| !tag.is_empty()) else {
        return "auto".to_owned();
    };
    let base = tag
        .split(['-', '_'])
        .next()
        .unwrap_or(tag)
        .to_ascii_lowercase();
    // It reaches a command line: only a plain language code gets through.
    if (2..=3).contains(&base.len()) && base.chars().all(|c| c.is_ascii_lowercase()) {
        base
    } else {
        "auto".to_owned()
    }
}

/// The language whisper reports having detected (`auto-detected language: zh (p = 0.76)`).
fn detected_language(log: &str) -> Option<String> {
    let rest = log.split("auto-detected language:").nth(1)?;
    let code = rest.split_whitespace().next()?;
    (code.len() <= 3 && code.chars().all(|c| c.is_ascii_lowercase())).then(|| code.to_owned())
}

/// Seconds of audio in a 16-bit PCM WAV, from its header.
fn wav_seconds(wav: &[u8]) -> f64 {
    if wav.len() < 44 {
        return 0.0;
    }
    let channels = u16::from_le_bytes([wav[22], wav[23]]).max(1);
    let rate = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]).max(1);
    let bits = u16::from_le_bytes([wav[34], wav[35]]).max(8);
    let bytes_per_second = f64::from(rate) * f64::from(channels) * f64::from(bits / 8);
    (wav.len() - 44) as f64 / bytes_per_second
}

/// The encoder's context, sized to the recording.
///
/// whisper encodes a fixed 30-second window (1500 frames, 50 per second) however short the
/// audio is; that is most of the cost of a two-second phrase, and it is what made the live
/// preview lag by seconds. A window just past the audio gives the same words several times
/// faster. Long recordings keep the full window.
fn audio_context(seconds: f64) -> Option<u32> {
    const FULL: f64 = 1500.0;
    let frames = (seconds * 50.0).ceil() + 128.0;
    (frames < FULL).then(|| frames.max(256.0) as u32)
}

/// One whisper.cpp invocation.
struct WhisperRun<'a> {
    model: &'a Path,
    output_stem: &'a Path,
    audio: &'a Path,
    language: &'a str,
    threads: u32,
    prompt: &'a str,
    audio_ctx: Option<u32>,
    greedy: bool,
}

fn whisper_args(run: &WhisperRun<'_>) -> Vec<String> {
    let WhisperRun {
        model,
        output_stem,
        audio,
        language,
        threads,
        prompt,
        audio_ctx,
        greedy,
    } = *run;
    // No `-np` / `--vad`: newer than some builds, and an unknown flag aborts the CLI.
    let mut args = vec![
        "-m".to_owned(),
        model.display().to_string(),
        "-l".to_owned(),
        language.to_owned(),
        "-nt".to_owned(),
        "-otxt".to_owned(),
        "-of".to_owned(),
        output_stem.display().to_string(),
        "-t".to_owned(),
        threads.to_string(),
    ];
    // Greedy decoding is several times faster than the default beam search: right for a
    // preview that is replaced a second later.
    if greedy {
        args.extend(["-bs", "1", "-bo", "1"].map(str::to_owned));
    }
    if let Some(frames) = audio_ctx {
        args.push("-ac".to_owned());
        args.push(frames.to_string());
    }
    if !prompt.trim().is_empty() {
        args.push("--prompt".to_owned());
        args.push(prompt.to_owned());
    }
    args.push(audio.display().to_string());
    args
}

/// Keep the last failed invocation on disk; overwritten every time so it cannot grow.
async fn record_failure(global: &Path, binary: &Path, args: &[String], detail: &str) {
    let dir = global.join("asr");
    if tokio::fs::create_dir_all(&dir).await.is_err() {
        return;
    }
    let tail: String = detail
        .chars()
        .rev()
        .take(4000)
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    let body = format!("{} {}\n\n{tail}", binary.display(), args.join(" "));
    let _ = tokio::fs::write(dir.join("last-command.log"), body).await;
}

/// Reject an empty or non-WAV recording before paying for a process spawn.
fn require_wav(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 128 {
        return Err("没有录到声音，请再试一次。".to_owned());
    }
    if bytes.len() > MAX_AUDIO_BYTES {
        return Err("录音太长了，请分几段说。".to_owned());
    }
    if &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(GENERIC_FAILURE.to_owned());
    }
    Ok(())
}

/// `whisper-cli` prints this and exits 0 when it is given no input file.
fn looks_like_usage(text: &str) -> bool {
    text.contains("usage:") && (text.contains("--help") || text.contains("supported audio formats"))
}

/// The transcript, on one line, without what whisper writes for silence and without the prompt
/// echoed back (whole, or a clause of it at either end: "……，使用简体中文。").
fn clean_transcript(text: &str, prompt: &str) -> String {
    let joined = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut out = joined.replace("[BLANK_AUDIO]", "");
    let prompt = prompt.trim();
    if !prompt.is_empty() {
        out = out.replace(prompt, "");
        out = strip_prompt_clauses(&out, prompt);
    }
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    // Near-silence also comes back as a caption-like aside -- "(字幕:)", "[音乐]", "(笑)". A
    // transcript that is nothing but one bracketed aside was never said.
    let bracketed = |open: char, close: char| out.starts_with(open) && out.ends_with(close);
    if bracketed('(', ')') || bracketed('（', '）') || bracketed('[', ']') || bracketed('【', '】')
    {
        return String::new();
    }
    out
}

/// Remove clauses of the prompt from the start and end of `text`, with their punctuation.
fn strip_prompt_clauses(text: &str, prompt: &str) -> String {
    const PUNCTUATION: &[char] = &['，', '。', ',', '.', '、', '！', '？', ' '];
    let clauses: Vec<&str> = prompt
        .split(PUNCTUATION)
        .map(str::trim)
        .filter(|clause| clause.chars().count() >= 3)
        .collect();
    let mut out = text.trim().to_owned();
    loop {
        let before = out.clone();
        for clause in &clauses {
            if let Some(rest) = out.strip_suffix(PUNCTUATION).unwrap_or(&out).strip_suffix(clause) {
                out = rest.trim_end_matches(PUNCTUATION).to_owned();
                // Keep the sentence's own full stop if the echo replaced it.
                if !out.is_empty() && text.trim_end().ends_with('。') {
                    out.push('。');
                }
            }
            if let Some(rest) = out.strip_prefix(clause) {
                out = rest.trim_start_matches(PUNCTUATION).to_owned();
            }
        }
        if out == before {
            return out;
        }
    }
}

/// Worker count: available cores, capped so the server stays responsive.
fn default_threads() -> u32 {
    std::thread::available_parallelism()
        .map_or(4, |count| u32::try_from(count.get()).unwrap_or(4))
        .min(8)
}

/// Configured path first, else the first known name in any search directory.
fn resolve_local_binary(config: &AsrConfig, global: &Path) -> Option<PathBuf> {
    let dirs = binary_search_dirs(global);
    if let Some(configured) = config.local_binary.as_deref() {
        let path = PathBuf::from(configured);
        if configured.contains('/') || configured.contains('\\') {
            return path.is_file().then_some(path);
        }
        return which(configured, &dirs);
    }
    LOCAL_BINARY_CANDIDATES
        .iter()
        .find_map(|name| which(name, &dirs))
}

fn binary_search_dirs(global: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![global.join("asr").join("bin")];
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.extend(BINARY_DIRS.iter().map(PathBuf::from));
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local").join("bin"));
    }
    dirs
}

/// Configured path, then Shirley's own folder, then wherever a whisper.cpp install keeps models.
fn resolve_local_model(global: &Path, configured: Option<&str>) -> Option<PathBuf> {
    if let Some(configured) = configured {
        let path = PathBuf::from(configured);
        if path.is_file() {
            return Some(path);
        }
    }
    let mut dirs = vec![global.join("asr").join("models"), global.join("asr")];
    dirs.extend(MODEL_DIRS.iter().map(PathBuf::from));
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".cache").join("whisper"));
    }
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut models: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| is_ggml_model(path))
            .collect();
        // An English-only model (`ggml-*.en.bin`) ignores `-l`; prefer a multilingual one.
        models.sort_by(|a, b| {
            is_english_only(a)
                .cmp(&is_english_only(b))
                .then_with(|| a.cmp(b))
        });
        if let Some(model) = models.into_iter().next() {
            return Some(model);
        }
    }
    None
}

fn is_ggml_model(path: &Path) -> bool {
    let named = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("ggml-") && name.ends_with(".bin"));
    named && path.metadata().is_ok_and(|meta| meta.len() > 1024 * 1024)
}

fn is_english_only(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains(".en."))
}

/// Return a usable model, downloading Shirley's own copy the first time.
async fn ensure_model(global: &Path, config: &AsrConfig) -> Result<PathBuf, String> {
    if let Some(path) = resolve_local_model(global, config.local_model.as_deref()) {
        return Ok(path);
    }
    let _guard = download_lock().lock().await;
    // Another request may have finished the download while this one waited.
    if let Some(path) = resolve_local_model(global, config.local_model.as_deref()) {
        return Ok(path);
    }
    let dir = global.join("asr").join("models");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|_error| "无法创建语音模型目录。".to_owned())?;
    let target = dir.join(DEFAULT_MODEL_FILE);
    let urls: Vec<String> = match config.model_url.as_deref() {
        Some(url) => vec![url.to_owned()],
        None => DEFAULT_MODEL_URLS
            .iter()
            .map(|base| format!("{base}/{DEFAULT_MODEL_FILE}"))
            .collect(),
    };
    let client = reqwest::Client::builder()
        .timeout(MODEL_DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|_error| "语音模型下载失败。".to_owned())?;
    let mut bytes = None;
    for url in urls {
        let Ok(response) = client.get(&url).send().await else {
            continue;
        };
        if !response.status().is_success() {
            continue;
        }
        let Ok(body) = response.bytes().await else {
            continue;
        };
        if body.len() >= MIN_MODEL_BYTES {
            bytes = Some(body);
            break;
        }
    }
    let Some(bytes) = bytes else {
        return Err(
            "语音模型下载失败，请检查服务器网络（或用 [asr] model_url 指定地址）。".to_owned(),
        );
    };
    // Write beside the target and rename: an interrupted download never passes for a model.
    let partial = dir.join(format!(".{DEFAULT_MODEL_FILE}.part"));
    tokio::fs::write(&partial, &bytes)
        .await
        .map_err(|_error| "语音模型保存失败。".to_owned())?;
    tokio::fs::rename(&partial, &target)
        .await
        .map_err(|_error| "语音模型保存失败。".to_owned())?;
    Ok(target)
}

fn which(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shirley-asr-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_transcript_drops_silence_markers_and_the_prompt() {
        let clean = |text: &str| clean_transcript(text, DEFAULT_PROMPT);
        assert_eq!(clean(" 你好\n\n世界 \n"), "你好 世界");
        assert_eq!(clean("[BLANK_AUDIO]\n"), "");
        assert_eq!(clean(DEFAULT_PROMPT), "");
        assert_eq!(clean("(字幕:)"), "");
        assert_eq!(clean("（笑）"), "");
        assert_eq!(clean("明天(周二)开会"), "明天(周二)开会");
        // A clause of the prompt echoed onto what was said.
        assert_eq!(clean("你有什么技能，使用简体中文。"), "你有什么技能。");
        assert_eq!(clean("你有什么技能使用简体中文"), "你有什么技能");
        assert_eq!(clean("以下是普通话的句子，今天天气不错"), "今天天气不错");
        // Words that merely appear inside the sentence stay.
        assert_eq!(clean("请用简体中文回答"), "请用简体中文回答");
        assert_eq!(clean_transcript("你好，使用简体中文", ""), "你好，使用简体中文");
    }

    #[test]
    fn detection_stays_within_the_languages_the_user_speaks() {
        let allowed = allowed_languages(None);
        assert_eq!(allowed, ["zh", "en"]);
        assert_eq!(constrain_language(Some("ja".into()), &allowed).as_deref(), Some("zh"));
        assert_eq!(constrain_language(Some("en".into()), &allowed).as_deref(), Some("en"));
        assert_eq!(constrain_language(None, &allowed), None);
        assert_eq!(allowed_languages(Some(vec!["en-US".into(), "bad;x".into()])), ["en"]);
        assert_eq!(allowed_languages(Some(vec![])), ["zh", "en"]);
    }

    #[test]
    fn the_encoder_window_follows_the_recording() {
        let mut wav = Vec::from(&b"RIFF\0\0\0\0WAVEfmt "[..]);
        wav.resize(44, 0);
        wav[22] = 1; // mono
        wav[24..28].copy_from_slice(&16_000_u32.to_le_bytes());
        wav[34] = 16;
        wav.resize(44 + 16_000 * 2 * 3, 0); // three seconds
        assert!((wav_seconds(&wav) - 3.0).abs() < 0.01);
        assert_eq!(audio_context(3.0), Some(278));
        assert_eq!(audio_context(1.0), Some(256));
        assert_eq!(audio_context(40.0), None);
    }

    #[test]
    fn usage_output_is_not_mistaken_for_a_transcript() {
        let help = "usage: whisper-cli [options] file0 file1 ...\n\
             supported audio formats: flac, mp3, ogg, wav\n  -h, --help\n";
        assert!(looks_like_usage(help));
        assert!(!looks_like_usage("Hello, this is Shirley."));
    }

    #[test]
    fn non_wav_recordings_are_rejected_before_spawning() {
        assert!(require_wav(b"").is_err());
        assert!(require_wav(&[0u8; 200]).is_err());
        let mut wav = Vec::from(&b"RIFF\0\0\0\0WAVE"[..]);
        wav.resize(200, 0);
        assert!(require_wav(&wav).is_ok());
    }

    #[test]
    fn the_language_is_detected_unless_pinned() {
        assert_eq!(whisper_language(None), "auto");
        assert_eq!(whisper_language(Some("")), "auto");
        assert_eq!(whisper_language(Some("zh-CN")), "zh");
        assert_eq!(whisper_language(Some("en_US")), "en");
        assert_eq!(whisper_language(Some("zh; rm -rf /")), "auto");
    }

    #[test]
    fn the_detected_language_is_read_from_the_log() {
        let log = "whisper_full_with_state: auto-detected language: zh (p = 0.762857)\n";
        assert_eq!(detected_language(log).as_deref(), Some("zh"));
        assert_eq!(detected_language("nothing here"), None);
    }

    #[test]
    fn the_prompt_goes_before_the_audio_file() {
        let args = whisper_args(&WhisperRun {
            model: Path::new("m.bin"),
            output_stem: Path::new("out"),
            audio: Path::new("a.wav"),
            language: "auto",
            threads: 4,
            prompt: DEFAULT_PROMPT,
            audio_ctx: Some(300),
            greedy: true,
        });
        assert_eq!(args.last().map(String::as_str), Some("a.wav"));
        assert!(args.windows(2).any(|pair| pair[0] == "--prompt"));
        assert!(args.windows(2).any(|pair| pair == ["-bs", "1"]));
    }

    #[tokio::test]
    async fn a_short_partial_is_skipped_without_running_anything() {
        let root = temp_root();
        let result = transcribe(&root, &[0u8; 1000], true, None).await;
        assert!(matches!(result, Ok(ref r) if r.text.is_empty()));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn model_lookup_prefers_multilingual_and_ignores_fragments() {
        let root = temp_root();
        let managed = root.join("asr").join("models");
        std::fs::create_dir_all(&managed).unwrap();
        std::fs::write(managed.join("ggml-tiny.bin"), b"nope").unwrap();
        assert!(resolve_local_model(&root, None).is_none());
        let english = managed.join("ggml-base.en.bin");
        std::fs::write(&english, vec![0_u8; 2 * 1024 * 1024]).unwrap();
        let multilingual = managed.join("ggml-base-q5_1.bin");
        std::fs::write(&multilingual, vec![0_u8; 2 * 1024 * 1024]).unwrap();
        assert_eq!(resolve_local_model(&root, None), Some(multilingual));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn binary_lookup_probes_the_managed_folder() {
        let root = temp_root();
        let bin = root.join("asr").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("whisper-cli");
        std::fs::write(&fake, b"#!/bin/sh\n").unwrap();
        assert_eq!(
            resolve_local_binary(&AsrConfig::default(), &root),
            Some(fake)
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
