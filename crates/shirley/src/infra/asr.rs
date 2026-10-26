//! Voice input (speech-to-text) for the composer's microphone button.
//!
//! **Recording and transcription both happen on the user's own device.** No audio ever
//! reaches a Shirley server -- the cloud runtime is text-only for dictation, and a phone that
//! opens a `cloud::` project dictates with its own browser recogniser. Two device-local paths
//! exist and the UI picks between them without asking:
//!
//! - **web**: the browser's own recogniser (`webkitSpeechRecognition`), which never reaches
//!   Rust at all (`web/src/lib/voice.ts`). This is what a phone or a plain browser uses.
//! - **local**: the `whisper.cpp` CLI of the computer the app runs on, reached through a
//!   Tauri command -- i.e. a process on that same computer. The binary is auto-detected and
//!   the ggml model comes from `[asr] local_model`, `SHIRLEY_ASR_LOCAL_MODEL`, or the managed
//!   copy in `<global>/asr/models`. No `ffmpeg` is involved because the page uploads 16 kHz
//!   mono WAV directly.
//!
//! There is deliberately **no** provider ("cloud") transcription engine: shipping an
//! utterance to a third party is a privacy regression, and it was a dead end anyway -- most
//! OpenAI-compatible providers do not implement `/audio/transcriptions`.
//!
//! **Shirley ships the model, not the runner.** A ggml model is the part users cannot
//! plausibly produce themselves, so it is downloaded once into `<global>/asr/models`
//! (57 MiB, multilingual) and then reused offline. The runner is borrowed from the machine,
//! because a self-contained runner would mean compiling `whisper.cpp` in the build -- that
//! needs `cmake` on every machine that builds the desktop feature, a worse trade than one
//! `brew install whisper-cpp`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::Serialize;

use super::settings::FileSettings;

/// One utterance cap. 24 MiB of 16 kHz mono PCM is roughly 12 minutes of speech.
const MAX_AUDIO_BYTES: usize = 24 * 1024 * 1024;
/// Local whisper on CPU is slow, but an unbounded child process is worse.
const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(180);
/// Local CLI candidates, tried in order when `local_binary` is not configured.
const LOCAL_BINARY_CANDIDATES: [&str; 3] = ["whisper-cli", "whisper-cpp", "whisper"];
/// Directories probed for the CLI in addition to `PATH`. A GUI-launched app does **not**
/// inherit the shell `PATH` on macOS, so Homebrew's prefix has to be named explicitly --
/// without this, a machine that already has whisper.cpp looks empty to the app.
const BINARY_DIRS: [&str; 4] = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];
/// Directories probed for an existing ggml model, in order, before offering a download.
const MODEL_DIRS: [&str; 2] = [
    "/opt/homebrew/share/whisper-cpp/models",
    "/usr/local/share/whisper-cpp/models",
];
/// The model Shirley manages: `base` quantised to q5_1 is 57 MiB and multilingual. Smaller
/// (`tiny`) is too weak for Chinese; larger (`small`, 466 MiB) is too slow to download on a
/// first keystroke.
const DEFAULT_MODEL_FILE: &str = "ggml-base-q5_1.bin";
/// Where to fetch the managed model, tried in order.
///
/// The mirror comes first because the Hugging Face host is unreachable from mainland
/// China -- measured, not assumed: a direct `huggingface.co` download fails with
/// `SSL_ERROR_SYSCALL` while the mirror answers the same path normally. The official host
/// stays as the fallback for everyone else.
const DEFAULT_MODEL_URLS: [&str; 2] = [
    "https://hf-mirror.com/ggerganov/whisper.cpp/resolve/main",
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main",
];
/// Downloads are slow; transcription is not. Kept apart so a big model is not mistaken for
/// a hung request.
const MODEL_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// A truncated download must not be cached as a model.
const MIN_MODEL_BYTES: usize = 8 * 1024 * 1024;

/// What this device can do, computed on every call so installing whisper.cpp takes effect
/// without restarting the app.
#[derive(Debug, Clone, Serialize)]
pub struct AsrCapabilities {
    /// Whether this device can transcribe with its own `whisper.cpp` right now.
    pub local_available: bool,
    /// Resolved CLI path: shown in hints, and the first thing to ask for in a bug report.
    pub local_binary: Option<String>,
    /// Resolved model path, when one is already on disk.
    pub local_model: Option<String>,
    /// `false` means the first utterance downloads the model once.
    pub local_model_ready: bool,
    /// One short setup hint; empty when everything is ready.
    pub hint: String,
}

/// One transcription, ready to append to the composer.
#[derive(Debug, Clone, Serialize)]
pub struct AsrResult {
    /// What the model heard. An empty string is an error, never a result.
    pub text: String,
    /// Wall-clock cost of the transcription.
    pub elapsed_ms: u64,
}

/// Resolved ASR settings: the `[asr]` table with `SHIRLEY_ASR_*` environment overrides on
/// top (env wins, the same priority as the rest of the settings chain).
#[derive(Debug, Clone, Default)]
struct AsrConfig {
    local_binary: Option<String>,
    local_model: Option<String>,
    model_url: Option<String>,
    local_language: Option<String>,
    local_threads: Option<u32>,
}

impl AsrConfig {
    /// Read the `[asr]` table, then layer environment overrides. A missing or broken
    /// config file is not an error here -- ASR is an input convenience and must never
    /// block startup.
    fn load(env: &dyn Fn(&str) -> Option<String>) -> Self {
        let file = FileSettings::load_file(&crate::storage::config_path())
            .ok()
            .flatten()
            .map(|settings| settings.asr)
            .unwrap_or_default();
        let pick = |key: &str, value: Option<String>| {
            env(key).filter(|item| !item.trim().is_empty()).or(value)
        };
        Self {
            local_binary: pick("SHIRLEY_ASR_LOCAL_BINARY", file.local_binary),
            local_model: pick("SHIRLEY_ASR_LOCAL_MODEL", file.local_model),
            model_url: pick("SHIRLEY_ASR_MODEL_URL", file.model_url),
            local_language: file.local_language,
            local_threads: file.local_threads,
        }
    }
}

/// Report what this device can transcribe with, and what is missing.
pub fn capabilities(global: &Path) -> AsrCapabilities {
    let config = AsrConfig::load(&|key| std::env::var(key).ok());
    let binary = resolve_local_binary(&config, global);
    let model = resolve_local_model(global, config.local_model.as_deref());

    let hint = if binary.is_none() {
        // Naming the one-line fix beats reporting a capability as missing.
        "Local dictation needs whisper.cpp: `brew install whisper-cpp` (or set [asr] local_binary)."
            .to_string()
    } else if model.is_none() {
        "The speech model (57 MB) downloads on your first dictation, once.".to_string()
    } else {
        String::new()
    };

    AsrCapabilities {
        local_available: binary.is_some(),
        local_binary: binary.map(|path| path.display().to_string()),
        local_model_ready: model.is_some(),
        local_model: model.map(|path| path.display().to_string()),
        hint,
    }
}

/// Transcribe one utterance, on this machine, with this machine's model.
///
/// `audio_base64` is base64 WAV (a `data:` URL prefix is tolerated). The returned text is
/// what the composer appends at the cursor.
///
/// # Errors
/// Returns a short user-facing message when whisper.cpp is missing, the audio is unusable,
/// or the CLI fails.
pub async fn transcribe(
    global: &Path,
    audio_base64: &str,
    language: Option<&str>,
) -> Result<AsrResult, String> {
    let started = Instant::now();
    let bytes = decode_audio(audio_base64)?;
    let config = AsrConfig::load(&|key| std::env::var(key).ok());
    let language = language
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or(config.local_language.as_deref());
    let text = transcribe_local(&config, global, &bytes, language).await?;
    Ok(AsrResult {
        text,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

/// Decode the uploaded audio, tolerating a `data:` URL prefix.
fn decode_audio(audio_base64: &str) -> Result<Vec<u8>, String> {
    let payload = audio_base64
        .rsplit_once("base64,")
        .map_or(audio_base64, |(_, tail)| tail)
        .trim();
    if payload.is_empty() {
        return Err("Nothing was recorded.".to_string());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|_error| "The recording could not be decoded.".to_string())?;
    if bytes.is_empty() {
        return Err("Nothing was recorded.".to_string());
    }
    if bytes.len() > MAX_AUDIO_BYTES {
        return Err("The recording is too long; split it into shorter pieces.".to_string());
    }
    Ok(bytes)
}

/// One short line for the composer when nothing more specific can be said. The technical
/// reason goes to `<global>/asr/last-command.log` instead of into the user's face.
const GENERIC_FAILURE: &str = "Voice input failed; try again.";

/// Run the local `whisper.cpp` CLI over a temp WAV file.
///
/// Two rules learned from real failures:
///
/// * The audio file goes in as a **positional** argument. That is the only form every
///   whisper.cpp release accepts, and when the CLI does not get an input file it prints its
///   **usage** to stdout and exits successfully — text we must never mistake for speech.
/// * The transcript is read from the file `-otxt` writes, never from stdout, which is shared
///   with the CLI's own logging.
async fn transcribe_local(
    config: &AsrConfig,
    global: &Path,
    bytes: &[u8],
    language: Option<&str>,
) -> Result<String, String> {
    let binary = resolve_local_binary(config, global)
        .ok_or_else(|| "Voice input is not set up on this machine.".to_string())?;
    let model = ensure_model(global, config).await?;
    require_wav(bytes)?;

    let dir = global.join("asr").join("tmp");
    if tokio::fs::create_dir_all(&dir).await.is_err() {
        return Err(GENERIC_FAILURE.to_string());
    }
    let stem = uuid::Uuid::new_v4().to_string();
    let audio_path = dir.join(format!("utterance-{stem}.wav"));
    // `-of` takes a path *without* extension: the CLI appends `.txt` itself.
    let output_stem = dir.join(format!("transcript-{stem}"));
    let transcript_path = output_stem.with_extension("txt");
    if tokio::fs::write(&audio_path, bytes).await.is_err() {
        return Err(GENERIC_FAILURE.to_string());
    }

    let threads = config.local_threads.unwrap_or_else(default_threads);
    let mut language = whisper_language(language);
    let mut attempt = 0;
    let (args, output) = loop {
        let args = whisper_args(&model, &output_stem, &audio_path, &language, threads);
        let run = tokio::process::Command::new(&binary)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .output();
        let output = match tokio::time::timeout(TRANSCRIBE_TIMEOUT, run).await {
            Err(_elapsed) => {
                let _ = tokio::fs::remove_file(&audio_path).await;
                return Err("Voice input took too long; try a shorter recording.".to_string());
            }
            Ok(Err(error)) => {
                let _ = tokio::fs::remove_file(&audio_path).await;
                record_failure(global, &binary, &args, &format!("spawn failed: {error}")).await;
                return Err(GENERIC_FAILURE.to_string());
            }
            Ok(Ok(output)) => output,
        };

        let written = tokio::fs::read_to_string(&transcript_path).await.ok();
        let text = written
            .as_deref()
            .map(clean_transcript_str)
            .filter(|text| !text.is_empty())
            .unwrap_or_default();
        if !text.is_empty() {
            let _ = tokio::fs::remove_file(&audio_path).await;
            let _ = tokio::fs::remove_file(&transcript_path).await;
            return Ok(text);
        }

        let noise = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        // This build does not know the language we asked for. `auto` always works, and a wrong
        // guess about the language must not cost the user their recording.
        if attempt == 0 && language != "auto" && noise.contains("unknown language") {
            language = "auto".to_string();
            attempt += 1;
            continue;
        }
        break (args, output);
    };

    let _ = tokio::fs::remove_file(&audio_path).await;
    let _ = tokio::fs::remove_file(&transcript_path).await;

    // Nothing to transcribe: keep the evidence, tell the user one sentence.
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
            "audio: {} bytes, language: {}\nstatus: {}\n{noise}",
            bytes.len(),
            language,
            output.status
        ),
    )
    .await;
    Err(if looks_like_usage(&noise) {
        // The CLI ran without an input file. From the user's side that is simply a failure;
        // the log above says which argument went missing.
        GENERIC_FAILURE
    } else if output.status.success() {
        "Voice input heard nothing; try again."
    } else {
        GENERIC_FAILURE
    }
    .to_string())
}

/// `whisper.cpp` only knows **base** language codes: `en`, `zh`, `ja`. A full BCP-47 tag
/// (`en-US`, `zh-CN`, which is exactly what `navigator.language` returns) makes it answer
/// `error: unknown language` — and then it prints its usage and exits **successfully**, which
/// looks like a broken CLI instead of a wrong argument.
fn whisper_language(tag: Option<&str>) -> String {
    let Some(tag) = tag.map(str::trim).filter(|tag| !tag.is_empty()) else {
        return "auto".to_string();
    };
    if tag.eq_ignore_ascii_case("auto") {
        return "auto".to_string();
    }
    let base = tag
        .split(['-', '_'])
        .next()
        .unwrap_or(tag)
        .to_ascii_lowercase();
    if base.is_empty() {
        "auto".to_string()
    } else {
        base
    }
}

fn whisper_args(
    model: &Path,
    output_stem: &Path,
    audio: &Path,
    language: &str,
    threads: u32,
) -> Vec<String> {
    // No `-np` / `--vad`: those are newer than some installed builds, and an unknown flag
    // makes the CLI abort instead of being ignored.
    vec![
        "-m".to_string(),
        model.display().to_string(),
        "-l".to_string(),
        language.to_string(),
        "-nt".to_string(),
        "-otxt".to_string(),
        "-of".to_string(),
        output_stem.display().to_string(),
        "-t".to_string(),
        threads.to_string(),
        audio.display().to_string(),
    ]
}

/// Keep the last failed invocation on disk (argv, exit status, CLI output).
///
/// A dictation failure is reported as one short sentence, which is right for the composer but
/// useless for diagnosing a machine we cannot see. This file is the missing half: it is only
/// ever read by hand, and it is overwritten on every failure so it cannot grow.
async fn record_failure(global: &Path, binary: &Path, args: &[String], detail: &str) {
    let dir = global.join("asr");
    if tokio::fs::create_dir_all(&dir).await.is_err() {
        return;
    }
    // `whisper.cpp` logs hundreds of lines; the tail is where the error is.
    let detail: String = detail
        .chars()
        .rev()
        .take(4000)
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    let body = format!(
        "binary: {}\nargs: {} {}\n\n{}",
        binary.display(),
        binary.display(),
        args.join(" "),
        detail
    );
    let _ = tokio::fs::write(dir.join("last-command.log"), body).await;
}

/// Reject an empty or non-WAV recording before paying for a process spawn.
fn require_wav(bytes: &[u8]) -> Result<(), String> {
    // Smallest plausible WAV: 44-byte header plus a few samples of audio.
    if bytes.len() < 128 {
        return Err("The recording was empty; try again.".to_string());
    }
    if &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(GENERIC_FAILURE.to_string());
    }
    Ok(())
}

/// `whisper-cli` prints this and exits 0 when it is given no input file.
fn looks_like_usage(text: &str) -> bool {
    text.contains("usage:") && (text.contains("--help") || text.contains("supported audio formats"))
}

/// Keep transcript lines, drop the CLI's own log noise.
fn clean_transcript(raw: &[u8]) -> String {
    clean_transcript_str(&String::from_utf8_lossy(raw))
}

fn clean_transcript_str(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| {
            // whisper.cpp logs are prefixed with the subsystem name (`whisper_`,
            // `ggml_`, `main:`) or are a bare timing report.
            !(line.starts_with("whisper_")
                || line.starts_with("ggml_")
                || line.starts_with("main:")
                || line.starts_with("system_info")
                || line.starts_with("error:"))
        })
        .collect();
    lines
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Default worker count: physical parallelism, capped so the machine stays usable.
fn default_threads() -> u32 {
    std::thread::available_parallelism()
        .map_or(4, |count| u32::try_from(count.get()).unwrap_or(4))
        .min(8)
}

/// Resolve the local CLI: configured path first, else the first known name in any search
/// directory.
///
/// `PATH` alone is not enough. The desktop app is launched from Finder or `cargo desktop`,
/// neither of which inherits the interactive shell's `PATH`, so a machine with
/// `brew install whisper-cpp` would otherwise look like it has no speech engine at all --
/// which is exactly how a working setup ended up falling through to the cloud engine.
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

/// Every directory the CLI may live in, most specific first.
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

/// Resolve an existing ggml model: configured path, then Shirley's own folder, then
/// wherever a `whisper.cpp` install keeps its models.
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
        models.sort();
        if let Some(model) = models.into_iter().next() {
            return Some(model);
        }
    }
    None
}

/// A `ggml-*.bin` file big enough to be a model rather than a stray fragment.
fn is_ggml_model(path: &Path) -> bool {
    let named = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("ggml-") && name.ends_with(".bin"));
    named && path.metadata().is_ok_and(|meta| meta.len() > 1024 * 1024)
}

/// Return a usable model, downloading Shirley's own copy the first time.
///
/// This is the "own model" half of dictation: the runner is borrowed from the machine, but
/// the weights are provided here, so a fresh install dictates without the user hunting for
/// a model file. Downloaded once into `<global>/asr/models`, then reused offline.
async fn ensure_model(global: &Path, config: &AsrConfig) -> Result<PathBuf, String> {
    if let Some(path) = resolve_local_model(global, config.local_model.as_deref()) {
        return Ok(path);
    }
    let dir = global.join("asr").join("models");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|_error| "Dictation cannot create its model folder.".to_string())?;
    let target = dir.join(DEFAULT_MODEL_FILE);
    // An explicit `model_url` is the whole list: the user picked a host on purpose.
    let urls: Vec<String> = match config.model_url.as_deref() {
        Some(url) => vec![url.to_string()],
        None => DEFAULT_MODEL_URLS
            .iter()
            .map(|base| format!("{base}/{DEFAULT_MODEL_FILE}"))
            .collect(),
    };
    let client = reqwest::Client::builder()
        .timeout(MODEL_DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|_error| "The speech model cannot be downloaded.".to_string())?;
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
        return Err("The speech model could not be fetched; check the network.".to_string());
    };
    // Write beside the target and rename, so an interrupted download never leaves a
    // truncated file that later passes for a working model.
    let partial = dir.join(format!(".{DEFAULT_MODEL_FILE}.part"));
    tokio::fs::write(&partial, &bytes)
        .await
        .map_err(|_error| "The speech model cannot be saved.".to_string())?;
    tokio::fs::rename(&partial, &target)
        .await
        .map_err(|_error| "The speech model cannot be saved.".to_string())?;
    Ok(target)
}

/// `which`, but over an explicit directory list.
fn which(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_audio_accepts_data_urls_and_rejects_garbage() {
        let payload = base64::engine::general_purpose::STANDARD.encode([1_u8, 2, 3]);
        assert_eq!(
            decode_audio(&format!("data:audio/wav;base64,{payload}")).unwrap(),
            vec![1, 2, 3]
        );
        assert_eq!(decode_audio(&payload).unwrap(), vec![1, 2, 3]);
        assert!(decode_audio("   ").is_err());
        assert!(decode_audio("not base64!!").is_err());
    }

    #[test]
    fn clean_transcript_drops_cli_noise() {
        let raw = b"whisper_init_from_file: loading\nhello world\n\nmain: processing\n";
        assert_eq!(clean_transcript(raw), "hello world");
        assert!(clean_transcript(b"whisper_init: x\n").is_empty());
    }

    #[test]
    fn usage_output_is_not_mistaken_for_a_transcript() {
        // Verbatim shape of what `whisper-cli -m <model>` (no input file) prints on stdout.
        let help = "usage: /opt/homebrew/bin/whisper-cli [options] file0 file1 ...\n\
             supported audio formats: flac, mp3, ogg, wav\n\
             options:\n  -h, --help [default] show this help message and exit\n";
        assert!(looks_like_usage(help));
        assert!(!looks_like_usage(
            "Hello, this is Shirley testing local dictation."
        ));
        assert!(!looks_like_usage(""));
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
    fn whisper_language_strips_the_region_subtag() {
        // `navigator.language` is a BCP-47 tag; whisper.cpp only knows base codes, and an
        // unknown one makes it print its usage and exit 0.
        assert_eq!(whisper_language(Some("en-US")), "en");
        assert_eq!(whisper_language(Some("zh-CN")), "zh");
        assert_eq!(whisper_language(Some("pt_BR")), "pt");
        assert_eq!(whisper_language(Some("ja")), "ja");
        assert_eq!(whisper_language(Some("AUTO")), "auto");
        assert_eq!(whisper_language(Some("")), "auto");
        assert_eq!(whisper_language(None), "auto");
    }

    #[test]
    fn capabilities_describe_this_device_only() {
        // Nothing here depends on a provider: dictation is device-local, so the only thing
        // the UI can learn is whether this machine can run whisper.cpp itself.
        let dir = std::env::temp_dir().join(format!("shirley-asr-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let caps = capabilities(&dir);
        assert!(!caps.hint.is_empty() || caps.local_available);
        assert_eq!(caps.local_available, caps.local_binary.is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn model_lookup_finds_managed_folder_and_ignores_fragments() {
        let root = std::env::temp_dir().join(format!("shirley-asr-{}", uuid::Uuid::new_v4()));
        let managed = root.join("asr").join("models");
        std::fs::create_dir_all(&managed).unwrap();
        assert!(resolve_local_model(&root, None).is_none());
        // A fragment under the size floor must not be mistaken for a model.
        let tiny = managed.join("ggml-tiny.bin");
        std::fs::write(&tiny, b"nope").unwrap();
        assert!(resolve_local_model(&root, None).is_none());
        let real = managed.join("ggml-base.bin");
        std::fs::write(&real, vec![0_u8; 2 * 1024 * 1024]).unwrap();
        assert_eq!(resolve_local_model(&root, None), Some(real));
        // An explicit path in the config wins over the managed folder.
        assert_eq!(
            resolve_local_model(&root, Some(tiny.to_str().unwrap())),
            Some(tiny)
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn binary_lookup_probes_beyond_path() {
        let root = std::env::temp_dir().join(format!("shirley-asr-{}", uuid::Uuid::new_v4()));
        let bin = root.join("asr").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("whisper-cli");
        std::fs::write(&fake, b"#!/bin/sh\n").unwrap();
        // A GUI launch has almost no PATH; the managed folder is probed regardless.
        assert_eq!(
            resolve_local_binary(&AsrConfig::default(), &root),
            Some(fake)
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
