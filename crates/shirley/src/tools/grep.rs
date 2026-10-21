//! Bounded workspace content search. Reuse ripgrep's ignore parser rather than
//! implementing gitignore precedence/negation ourselves or requiring an rg binary.

use std::fmt::Write as _;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use globset::Glob;
use ignore::WalkBuilder;
use regex::bytes::RegexBuilder;
use shirley_agent_sdk::workspace::{WorkSpace, WorkspaceError};
use shirley_agent_sdk::{ToolContext, ToolError, tool};

use super::read::{clip_to_budget, is_secret_file, resolve_workspace_roots};

const DEFAULT_RESULTS: usize = 100;
const MAX_RESULTS: usize = 500;
const MAX_OUTPUT_BYTES: usize = 32 * 1024;
const MAX_LINE_BYTES: usize = 1000;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_VISITED: usize = 50_000;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "these mirror independent search flags from the tool schema; a state machine would obscure the mapping"
)]
struct SearchOptions {
    path: Option<String>,
    glob: Option<String>,
    case_insensitive: bool,
    fixed_strings: bool,
    files_only: bool,
    max_results: Option<usize>,
    include_ignored: bool,
    include_hidden: bool,
}

#[tool(
    description = "Search workspace text with a regex (or fixed_strings=true for literal text). Use this instead of bash grep/rg. Respects nested .gitignore, .ignore, .rgignore and Git excludes, including outside Git repositories; glob only narrows results and cannot override ignores. Hidden files are excluded unless include_hidden=true. Ignored files require include_ignored=true, even for an explicit file path. Skips binaries, symlinks, suspected secrets, VCS/runtime state and files over 2 MiB. Returns path:line:text, or paths with files_only=true. Results have hard count, line-length, byte and scan limits; truncation is reported. No shell or external rg installation is needed."
)]
// The tool signature is the public schema; splitting it into a struct would change the
// model-facing JSON schema, so keep the flat argument list.
#[expect(
    clippy::too_many_arguments,
    reason = "tool parameter list is the public schema; grouping would change the model-facing shape"
)]
pub async fn grep(
    ctx: &ToolContext,
    #[param(
        description = "regex pattern; matched independently on each line; use fixed_strings=true for literal punctuation"
    )]
    pattern: String,
    #[param(
        description = "workspace-relative file or directory to search; defaults to the whole workspace"
    )]
    path: Option<String>,
    #[param(
        description = "optional include glob over workspace-relative paths, e.g. *.rs or src/**/*.tsx; does not override ignore rules"
    )]
    glob: Option<String>,
    #[param(description = "case-insensitive matching; defaults to false")] case_insensitive: Option<
        bool,
    >,
    #[param(description = "interpret pattern as literal text instead of regex; defaults to false")]
    fixed_strings: Option<bool>,
    #[param(description = "return matching file paths only; defaults to false")] files_only: Option<
        bool,
    >,
    #[param(
        description = "maximum matching lines (or files with files_only), defaults to 100, hard cap 500"
    )]
    max_results: Option<usize>,
    #[param(
        description = "explicitly search files excluded by ignore rules; defaults to false; hidden files still need include_hidden"
    )]
    include_ignored: Option<bool>,
    #[param(
        description = "search hidden files and directories; defaults to false; secrets and VCS/runtime state remain excluded"
    )]
    include_hidden: Option<bool>,
) -> Result<String, ToolError> {
    let roots = resolve_workspace_roots(ctx)
        .ok_or_else(|| ToolError::ExecutionError("cannot determine the workspace root".into()))?;
    let options = SearchOptions {
        path,
        glob,
        case_insensitive: case_insensitive.unwrap_or(false),
        fixed_strings: fixed_strings.unwrap_or(false),
        files_only: files_only.unwrap_or(false),
        max_results,
        include_ignored: include_ignored.unwrap_or(false),
        include_hidden: include_hidden.unwrap_or(false),
    };
    // Filesystem traversal and regex matching must not block the async executor.
    tokio::task::spawn_blocking(move || search_roots(&roots, &pattern, options))
        .await
        .map_err(|error| ToolError::ExecutionError(format!("search worker failed: {error}")))?
}

/// Multi-root search: a project may attach several local checkouts, so a relative `path` is
/// searched under whichever roots actually contain it. With a single root this is just [`search`].
#[expect(
    clippy::needless_pass_by_value,
    reason = "SearchOptions is moved into a spawn_blocking closure and read there"
)]
fn search_roots(
    roots: &[PathBuf],
    pattern: &str,
    options: SearchOptions,
) -> Result<String, ToolError> {
    let Some(primary) = roots.first() else {
        return Err(ToolError::ExecutionError(
            "cannot determine the workspace root".into(),
        ));
    };
    if roots.len() == 1 {
        return search(primary, pattern, options);
    }
    // Restrict to the roots that contain the requested path; a bare `.` means every root.
    let requested = options.path.as_deref().unwrap_or(".");
    let selected: Vec<&PathBuf> = roots
        .iter()
        .filter(|root| {
            WorkSpace::new((*root).clone())
                .ok()
                .and_then(|workspace| workspace.resolve(requested).ok())
                .is_some_and(|resolved| resolved.exists())
        })
        .collect();
    match selected.as_slice() {
        // Nothing contains it: search the primary so the error names a concrete path.
        [] => search(primary, pattern, options),
        [only] => search(only, pattern, options),
        many => {
            let mut combined = String::new();
            for root in many {
                let output = search(root, pattern, options.clone())?;
                // Drop the per-root empty placeholder; a real combined footer is added below.
                if output.trim() == "[no matches in searched files]" {
                    continue;
                }
                if combined.len() + output.len() > MAX_OUTPUT_BYTES {
                    combined.push_str("[results truncated: combined output byte limit reached]\n");
                    break;
                }
                combined.push_str(&output);
            }
            if combined.is_empty() {
                combined.push_str("[no matches in searched files]\n");
            }
            Ok(combined)
        }
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "SearchOptions is moved into a spawn_blocking closure and read there"
)]
fn search(root: &Path, pattern: &str, options: SearchOptions) -> Result<String, ToolError> {
    let deadline = Instant::now() + SEARCH_TIMEOUT;
    if pattern.is_empty() || pattern.len() > 16 * 1024 {
        return Err(ToolError::ArgumentsError(
            "pattern must contain 1..=16384 bytes".into(),
        ));
    }
    let limit = options.max_results.unwrap_or(DEFAULT_RESULTS);
    if limit == 0 {
        return Err(ToolError::ArgumentsError(
            "max_results must be greater than 0".into(),
        ));
    }
    let limit = limit.min(MAX_RESULTS);
    let expression = if options.fixed_strings {
        regex::escape(pattern)
    } else {
        pattern.to_owned()
    };
    let regex = RegexBuilder::new(&expression)
        .case_insensitive(options.case_insensitive)
        .size_limit(2 * 1024 * 1024)
        .build()
        .map_err(|error| ToolError::ArgumentsError(format!("invalid regex: {error}")))?;
    let glob = options
        .glob
        .as_deref()
        .map(|pattern| {
            Glob::new(pattern)
                .map(|glob| glob.compile_matcher())
                .map_err(|error| ToolError::ArgumentsError(format!("invalid glob: {error}")))
        })
        .transpose()?;

    let workspace = WorkSpace::new(root.to_path_buf())
        .map_err(|error| ToolError::ExecutionError(error.to_string()))?;
    let root = workspace.resolve(".").map_err(|e| path_error(&e))?;
    let scope = workspace
        .resolve(options.path.as_deref().unwrap_or("."))
        .map_err(|e| path_error(&e))?;
    let metadata = std::fs::metadata(&scope).map_err(|error| {
        ToolError::ExecutionError(format!("cannot search requested path: {error}"))
    })?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(ToolError::ArgumentsError(
            "search path must be a regular file or directory".into(),
        ));
    }

    // Always walk from the workspace root, even for a single file. Starting the
    // walker at an explicitly named ignored file would bypass its ignore rules.
    let mut walk = WalkBuilder::new(&root);
    walk.require_git(false)
        .follow_links(false)
        .hidden(!options.include_hidden)
        .ignore(!options.include_ignored)
        .git_ignore(!options.include_ignored)
        .git_exclude(!options.include_ignored)
        .git_global(!options.include_ignored);
    if !options.include_ignored {
        walk.add_custom_ignore_filename(".rgignore");
    }
    let include_hidden = options.include_hidden;
    walk.filter_entry(move |entry| {
        let path = entry.path();
        if path != scope && !scope.starts_with(path) && !path.starts_with(&scope) {
            return false;
        }
        let name = entry.file_name().to_string_lossy();
        entry.depth() == 0
            || ((include_hidden || !name.starts_with('.'))
                && !matches!(name.as_ref(), ".git" | ".hg" | ".svn" | ".shirley")
                && !is_secret_file(&name))
    });

    let mut output = String::new();
    let mut matches = 0;
    let mut skipped_large = 0;
    let mut errors = 0;
    let mut stopped = None;
    'scan: for (visited, entry) in walk.build().enumerate() {
        if Instant::now() >= deadline || visited >= MAX_VISITED {
            stopped = Some("scan limit reached");
            break;
        }
        let Ok(entry) = entry else {
            errors += 1;
            continue;
        };
        if entry.error().is_some() {
            errors += 1;
        }
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(&root)
            .expect("walker stays under root");
        if glob.as_ref().is_some_and(|glob| !glob.is_match(relative)) {
            continue;
        }
        let body = match read_bounded(entry.path()) {
            Ok(Some(body)) => body,
            Ok(None) => {
                skipped_large += 1;
                continue;
            }
            Err(_) => {
                errors += 1;
                continue;
            }
        };
        if body.contains(&0) {
            continue;
        }
        let display = relative.to_string_lossy().replace('\\', "/");
        for (line, text) in body.split_inclusive(|&byte| byte == b'\n').enumerate() {
            if Instant::now() >= deadline {
                stopped = Some("scan limit reached");
                break 'scan;
            }
            let text = text.strip_suffix(b"\n").unwrap_or(text);
            let text = text.strip_suffix(b"\r").unwrap_or(text);
            let Some(found) = regex.find(text) else {
                continue;
            };
            if matches >= limit {
                stopped = Some("result limit reached");
                break 'scan;
            }
            let result = if options.files_only {
                format!("{display}\n")
            } else {
                // Matching uses the whole bounded file, not just the preview: a
                // match near the end of a huge line is still found and reported.
                format!(
                    "{display}:{}:{}\n",
                    line + 1,
                    line_preview(text, found.start())
                )
            };
            // Reserve enough space for the status footer within the hard cap.
            if output.len() + result.len() > MAX_OUTPUT_BYTES - 512 {
                stopped = Some("output byte limit reached");
                break 'scan;
            }
            output.push_str(&result);
            matches += 1;
            if options.files_only {
                break;
            }
        }
    }
    if let Some(reason) = stopped {
        let _ = writeln!(
            output,
            "[results truncated: {reason}; narrow path/glob/pattern]"
        );
    } else if matches == 0 {
        output.push_str("[no matches in searched files]\n");
    }
    if skipped_large > 0 || errors > 0 {
        let _ = writeln!(
            output,
            "[partial search: {skipped_large} files over 2 MiB skipped; {errors} traversal/ignore/read errors]"
        );
    }
    Ok(output)
}

fn path_error(error: &WorkspaceError) -> ToolError {
    match error {
        WorkspaceError::OutsideRoot(_) | WorkspaceError::InvalidPath(_) => {
            ToolError::ArgumentsError(error.to_string())
        }
        WorkspaceError::Io(_) => ToolError::ExecutionError(error.to_string()),
    }
}

fn read_bounded(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    let file = File::open(path)?;
    if file.metadata()?.len() > MAX_FILE_BYTES {
        return Ok(None);
    }
    // The file can grow after metadata was checked. Never read it without a cap.
    let mut body = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut body)?;
    Ok((body.len() as u64 <= MAX_FILE_BYTES).then_some(body))
}

fn line_preview(line: &[u8], match_start: usize) -> String {
    // Center long-line excerpts near the first match instead of returning an
    // unrelated prefix. Decode only a bounded window, including for invalid UTF-8.
    let mut start = if match_start >= MAX_LINE_BYTES {
        match_start.saturating_sub(MAX_LINE_BYTES / 4)
    } else {
        0
    };
    while start > 0 && line[start] & 0xc0 == 0x80 {
        start -= 1;
    }
    let end = line.len().min(start + MAX_LINE_BYTES);
    let text = String::from_utf8_lossy(&line[start..end]);
    let clipped = clip_to_budget(&text, MAX_LINE_BYTES);
    if start > 0 || end < line.len() || clipped.len() < text.len() {
        format!(
            "{clipped} [line truncated; excerpt starts at byte {}]",
            start + 1
        )
    } else {
        clipped.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "shirley-grep-{}-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn write(&self, path: &str, contents: impl AsRef<[u8]>) {
            let path = self.0.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }

        fn search(&self, pattern: &str, options: SearchOptions) -> String {
            search(&self.0, pattern, options).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn nested_gitignore_and_negation_work_without_a_git_directory() {
        let f = Fixture::new();
        f.write(".gitignore", "*.log\n!keep.log\n");
        f.write("keep.log", "needle\n");
        f.write("skip.log", "needle\n");
        f.write("web/.gitignore", "node_modules/\ndist/\n");
        f.write("web/src/app.rs", "needle\n");
        f.write("web/node_modules/pkg/index.js", "needle\n");
        f.write("web/dist/index.js.map", "needle\n");
        let out = f.search("needle", SearchOptions::default());
        assert!(out.contains("keep.log:1:needle"));
        assert!(out.contains("web/src/app.rs:1:needle"));
        assert!(
            !out.contains("skip.log") && !out.contains("node_modules") && !out.contains("dist"),
            "{out}"
        );
        let out = f.search(
            "needle",
            SearchOptions {
                path: Some("web".into()),
                ..Default::default()
            },
        );
        assert!(out.contains("web/src/app.rs"));
        assert!(
            !out.contains("keep.log") && !out.contains("node_modules"),
            "{out}"
        );
    }

    #[test]
    fn explicit_paths_and_globs_do_not_bypass_ignores() {
        let f = Fixture::new();
        f.write(".gitignore", "ignored.rs\n");
        f.write("ignored.rs", "needle\n");
        f.write("src/a.rs", "needle\n");
        let out = f.search(
            "needle",
            SearchOptions {
                glob: Some("*.rs".into()),
                ..Default::default()
            },
        );
        assert!(out.contains("src/a.rs"));
        assert!(!out.contains("ignored.rs"));
        let out = f.search(
            "needle",
            SearchOptions {
                path: Some("ignored.rs".into()),
                ..Default::default()
            },
        );
        assert!(out.contains("no matches"));
        let out = f.search(
            "needle",
            SearchOptions {
                path: Some("ignored.rs".into()),
                include_ignored: true,
                ..Default::default()
            },
        );
        assert!(out.contains("ignored.rs:1:needle"));
    }

    #[test]
    fn ignore_and_rgignore_precedence_is_preserved() {
        let f = Fixture::new();
        f.write(".gitignore", "*.rs\n");
        f.write(".ignore", "!keep.rs\n!drop.rs\n");
        f.write(".rgignore", "drop.rs\n");
        f.write("keep.rs", "needle\n");
        f.write("drop.rs", "needle\n");
        f.write("other.rs", "needle\n");
        let out = f.search("needle", SearchOptions::default());
        assert!(out.contains("keep.rs:1:needle"));
        assert!(
            !out.contains("drop.rs") && !out.contains("other.rs"),
            "{out}"
        );
    }

    #[test]
    fn matches_regex_literals_case_and_line_numbers() {
        let f = Fixture::new();
        f.write("a.rs", "fn Alpha() {}\r\nneedle\n\n");
        let out = f.search(
            "FN (ALPHA|BETA)",
            SearchOptions {
                case_insensitive: true,
                ..Default::default()
            },
        );
        assert!(out.contains("a.rs:1:fn Alpha() {}"));
        let out = f.search(
            "Alpha()",
            SearchOptions {
                fixed_strings: true,
                ..Default::default()
            },
        );
        assert!(out.contains("a.rs:1:"));
        let out = f.search("^$", SearchOptions::default());
        assert_eq!(out, "a.rs:3:\n");
        assert!(
            f.search("missing", SearchOptions::default())
                .contains("no matches")
        );
    }

    #[test]
    fn files_only_and_global_result_limit() {
        let f = Fixture::new();
        f.write("a.txt", "needle\nneedle\nneedle\n");
        f.write("b.txt", "needle\n");
        let out = f.search(
            "needle",
            SearchOptions {
                files_only: true,
                ..Default::default()
            },
        );
        assert_eq!(out.lines().count(), 2);
        assert!(out.contains("a.txt\n") && out.contains("b.txt\n"));
        let out = f.search(
            "needle",
            SearchOptions {
                max_results: Some(2),
                ..Default::default()
            },
        );
        assert_eq!(
            out.lines().filter(|line| line.contains(":needle")).count(),
            2
        );
        assert!(out.contains("results truncated: result limit"));
    }

    #[test]
    fn caps_large_single_lines_total_bytes_and_requested_results() {
        let f = Fixture::new();
        // Match past the preview window, with multibyte text across its boundary.
        f.write("long.txt", format!("{}needle\n", "中".repeat(200_000)));
        let out = f.search("needle", SearchOptions::default());
        assert!(out.starts_with("long.txt:1:"));
        assert!(out.contains("needle"));
        assert!(out.contains("line truncated") && out.len() < 1200);
        f.write(
            "many.txt",
            format!("needle{}\n", "x".repeat(900)).repeat(1000),
        );
        let out = f.search(
            "needle",
            SearchOptions {
                max_results: Some(usize::MAX),
                ..Default::default()
            },
        );
        assert!(out.len() <= MAX_OUTPUT_BYTES);
        assert!(out.contains("output byte limit"));
        f.write("many.txt", "needle\n".repeat(1000));
        let out = f.search(
            "needle",
            SearchOptions {
                path: Some("many.txt".into()),
                max_results: Some(usize::MAX),
                ..Default::default()
            },
        );
        assert_eq!(
            out.lines()
                .filter(|line| line.starts_with("many.txt:"))
                .count(),
            MAX_RESULTS
        );
        assert!(out.contains("result limit"));
    }

    #[test]
    fn hidden_opt_in_never_enables_secrets_state_or_binaries() {
        let f = Fixture::new();
        // Ignore-rule whitelists must not bypass the explicit hidden-file switch.
        f.write(".ignore", "!.config/\n");
        f.write(".config/settings.txt", "needle\n");
        f.write(".env", "needle\n");
        f.write("nested/private.key", "needle\n");
        f.write(".git/config", "needle\n");
        f.write(".shirley/sessions/log", "needle\n");
        f.write("binary", b"needle\0");
        assert!(
            f.search("needle", SearchOptions::default())
                .contains("no matches")
        );
        let out = f.search(
            "needle",
            SearchOptions {
                include_hidden: true,
                include_ignored: true,
                ..Default::default()
            },
        );
        assert_eq!(out, ".config/settings.txt:1:needle\n");
    }

    #[test]
    fn skips_oversized_files_with_a_partial_search_notice() {
        let f = Fixture::new();
        f.write("big.map", vec![b'x'; MAX_FILE_BYTES as usize + 1]);
        let out = f.search("x", SearchOptions::default());
        assert!(out.contains("1 files over 2 MiB skipped"));
        assert!(out.len() < 200);
    }

    #[test]
    fn rejects_invalid_patterns_globs_limits_and_workspace_escapes() {
        let f = Fixture::new();
        for pattern in ["", "("] {
            assert!(matches!(
                search(&f.0, pattern, SearchOptions::default()),
                Err(ToolError::ArgumentsError(_))
            ));
        }
        for options in [
            SearchOptions {
                path: Some("../outside".into()),
                ..Default::default()
            },
            SearchOptions {
                path: Some(f.0.to_string_lossy().into_owned()),
                ..Default::default()
            },
            SearchOptions {
                glob: Some("[".into()),
                ..Default::default()
            },
            SearchOptions {
                max_results: Some(0),
                ..Default::default()
            },
        ] {
            assert!(matches!(
                search(&f.0, "needle", options),
                Err(ToolError::ArgumentsError(_))
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_symlinks_outside_the_workspace() {
        let f = Fixture::new();
        let outside = Fixture::new();
        outside.write("secret.txt", "needle\n");
        std::os::unix::fs::symlink(&outside.0, f.0.join("linked")).unwrap();
        assert!(
            f.search("needle", SearchOptions::default())
                .contains("no matches")
        );
        assert!(matches!(
            search(
                &f.0,
                "needle",
                SearchOptions {
                    path: Some("linked".into()),
                    ..Default::default()
                }
            ),
            Err(ToolError::ArgumentsError(_))
        ));
    }

    #[tokio::test]
    async fn registers_and_invokes_through_tool_manager() {
        use shirley_agent_sdk::{ToolCall, ToolManager};
        let mut manager = ToolManager::new();
        manager.register(grep::tool()).unwrap();
        let definitions = manager.definitions();
        let definition = definitions.iter().find(|tool| tool.name == "grep").unwrap();
        assert!(
            definition.parameters["properties"]
                .get("include_ignored")
                .is_some()
        );
        let call = ToolCall {
            id: "search".into(),
            name: "grep".into(),
            arguments: r#"{"pattern":"("}"#.into(),
        };
        assert!(matches!(
            manager.invoke(&call, manager.context().clone()).await,
            Err(ToolError::ArgumentsError(_))
        ));
    }
}
