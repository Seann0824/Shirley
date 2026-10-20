//! The system prompt: supports a fixed string, and also generation from runtime context.
//!
//! The reason for the "function form" is that the system prompt needs runtime information
//! folded in — most notably a coding agent's **current working directory** (`plan.md`:
//! without knowing the workspace, the AI starts searching from the filesystem root and
//! wastes tokens), plus the project's own **guide file** (`Agent.md` in this repo).
//!
//! This information is only known when `Agent` is constructed, and we want it "recomputed
//! on every rebuild", so the prompt is modeled as a
//! `Fn(&SystemPromptContext) -> String`. A caller can just pass a string for convenience,
//! or pass a function that reads the working directory and assembles the project guide.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

/// The runtime context available when building the system prompt.
///
/// Currently it carries only the "current working directory". If more runtime information
/// needs injecting later (e.g. platform, available-tool summary), add fields here; the
/// function signature need not change.
#[derive(Debug, Clone, Default)]
pub struct SystemPromptContext {
    /// The current working directory (the root of the coding agent's scope).
    ///
    /// `None` means the caller did not provide one — the prompt function must handle that
    /// case itself and must not assume it is always present.
    pub working_dir: Option<PathBuf>,
}

/// The system prompt.
///
/// It can be a fixed string (`From<String>` / `From<&str>`) or a function
/// (`From<F: Fn(&SystemPromptContext) -> String>`). The function form lets the prompt be
/// generated dynamically from the runtime environment, e.g. folding in the current working
/// directory and the project guide (`Agent.md`).
///
/// The reason for `Arc<dyn Fn>` rather than a generic parameter: `Agent` must store it in a
/// field and call it repeatedly on compaction rebuild. A generic would infect the whole
/// `Agent` with the type parameter, which isn't worth it. The function must be
/// `Send + Sync`, because the stream returned by `run_stream` is `Send`.
#[derive(Clone)]
pub struct SystemPrompt {
    resolve: Arc<dyn Fn(&SystemPromptContext) -> String + Send + Sync>,
}

impl SystemPrompt {
    /// Resolve the final system-prompt text against the current context.
    #[must_use]
    pub fn resolve(&self, context: &SystemPromptContext) -> String {
        (self.resolve)(context)
    }
}

impl Default for SystemPrompt {
    /// Defaults to an empty prompt — `Agent` uses that to decide "there is no system prompt"
    /// and does not prepend a System message.
    fn default() -> Self {
        Self::from(String::new())
    }
}

impl fmt::Debug for SystemPrompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SystemPrompt")
            .field("resolve", &"<fn>")
            .finish()
    }
}

impl From<String> for SystemPrompt {
    fn from(prompt: String) -> Self {
        Self {
            resolve: Arc::new(move |_| prompt.clone()),
        }
    }
}

impl From<&str> for SystemPrompt {
    fn from(prompt: &str) -> Self {
        Self::from(prompt.to_owned())
    }
}

impl<F> From<F> for SystemPrompt
where
    F: Fn(&SystemPromptContext) -> String + Send + Sync + 'static,
{
    fn from(resolve: F) -> Self {
        Self {
            resolve: Arc::new(resolve),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: Option<&str>) -> SystemPromptContext {
        SystemPromptContext {
            working_dir: dir.map(PathBuf::from),
        }
    }

    #[test]
    fn static_string_ignores_context() {
        let prompt = SystemPrompt::from("You are Shirley");
        assert_eq!(prompt.resolve(&ctx(None)), "You are Shirley");
        assert_eq!(prompt.resolve(&ctx(Some("/tmp"))), "You are Shirley");
    }

    #[test]
    fn str_slice_converts() {
        let prompt: SystemPrompt = "hello".into();
        assert_eq!(prompt.resolve(&ctx(None)), "hello");
    }

    #[test]
    fn function_receives_working_dir() {
        let prompt = SystemPrompt::from(|ctx: &SystemPromptContext| {
            format!(
                "cwd={}",
                ctx.working_dir
                    .as_deref()
                    .map_or_else(|| "<none>".into(), |p| p.display().to_string())
            )
        });
        assert_eq!(prompt.resolve(&ctx(Some("/work"))), "cwd=/work");
        assert_eq!(prompt.resolve(&ctx(None)), "cwd=<none>");
    }

    #[test]
    fn function_item_converts() {
        fn build(ctx: &SystemPromptContext) -> String {
            format!("dir:{:?}", ctx.working_dir)
        }
        let prompt = SystemPrompt::from(build);
        assert!(prompt.resolve(&ctx(Some("/a"))).contains("/a"));
    }

    #[test]
    fn default_resolves_to_empty() {
        let prompt = SystemPrompt::default();
        assert!(prompt.resolve(&ctx(Some("/a"))).is_empty());
    }

    #[test]
    fn resolve_is_repeatable() {
        // Compaction rebuild calls resolve repeatedly; the result must be stable and reentrant.
        let prompt = SystemPrompt::from(|_: &SystemPromptContext| "stable".to_string());
        assert_eq!(prompt.resolve(&ctx(None)), prompt.resolve(&ctx(None)));
    }
}
