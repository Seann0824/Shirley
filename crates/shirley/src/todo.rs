//! Task ledger (todo): task state maintained by the model itself, surviving context compaction.
//!
//! **This is an application-layer capability, not an SDK base capability.** The ledger belongs to "the coding agent product"
//! as a feature tradeoff (what to record, how to nag, what text to inject); the SDK only provides the generic seam
//! [`shirley_agent_sdk::ContextProvider`] -- "append one system message at the end of every request".
//! This module implements two things:
//!
//! - [`TodoTool`]: the `todo` tool exposed to the model (the model decides when to update it);
//! - [`TodoContextProvider`]: renders the ledger into one system message, appended **at the end of every request**
//!   via the `ContextProvider` seam (does not participate in compaction, survives compaction, does not touch the prefix cache).
//!
//! **Why it is needed**: compaction squashes the conversation into a lossy summary, so the model loses "where I got to and
//! what I already know", and re-explores and re-compacts, forming a positive feedback loop. The ledger records "goal / steps /
//! conclusions reached / open questions" **outside the conversation**, where compaction cannot touch it.
//!
//! The ledger is not "summarizing again" (that would be a second loss): what is written in is a fact the model itself confirmed.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use shirley_agent_sdk::{ContextProvider, ToolError};

/// Character cap when rendering the ledger into the injected text.
///
/// The ledger itself consumes context too; over the cap it is truncated and explicitly marked (so it does not monopolize the context).
const MAX_RENDER_CHARS: usize = 4000;

/// How many consecutive rounds without updating the ledger before the nag reminder is injected.
///
/// A value of 3 matches the open-source implementation (learn-claude-code): too early interrupts the normal rhythm,
/// too late and the drift has already happened.
pub const TODO_NAG_AFTER_ROUNDS: usize = 3;

/// In-memory storage for the task ledger.
///
/// Shared via `Arc` (the injection provider and the `todo` tool each hold one), internal mutability via `Mutex`.
/// Persistence is left out (lost when the process ends, unrelated to session logs).
pub struct TodoStore {
    inner: Mutex<TaskState>,
    revision: AtomicUsize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TaskState {
    goal: Option<String>,
    steps: Vec<Step>,
    findings: Vec<String>,
    open_questions: Vec<String>,
}

/// Completion state of a step (aligned with Claude Code `TodoWrite`'s three states).
///
/// `InProgress` is the "anti-drift" anchor: at most one step may be in this state at a time, forcing the model
/// to finish one thing before starting the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Step {
    text: String,
    status: TodoStatus,
}

/// Ledger update command (the `todo` tool's input).
///
/// **Patch semantics**: only handles explicitly provided fields; fields not provided are left as is.
/// - `goal` / `steps`: whole replacement (`steps` is a checklist; the model resends the full list every round, so it self-corrects naturally);
/// - `add_findings` / `add_open_questions`: append;
/// - `clear`: reset the entire ledger.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TodoUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<TodoStep>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_findings: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_open_questions: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clear: Option<bool>,
}

/// One step in the checklist.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TodoStep {
    pub text: String,
    /// Defaults to `pending`. Three states as in [`TodoStatus`]; at most one step `in_progress` at a time.
    #[serde(default)]
    pub status: TodoStatus,
}

/// Structured view of the task ledger for UI rendering (unrelated to the XML injected to the model).
///
/// `TodoStore` is pure in-memory state; the UI side needs a serializable read-only snapshot like "goal + step progress",
/// rather than parsing the injected text. An empty ledger (no goal and no steps) returns `None`, so the UI hides the task card.
#[derive(Debug, Clone, Serialize)]
pub struct TaskView {
    pub goal: Option<String>,
    pub steps: Vec<TaskStepView>,
    /// Number of completed steps (for the "3/7" progress display).
    pub completed: usize,
    pub total: usize,
    /// The step currently in progress (at most one).
    pub current: Option<String>,
}

/// One step in the view.
#[derive(Debug, Clone, Serialize)]
pub struct TaskStepView {
    pub text: String,
    pub status: TodoStatus,
}

impl TodoStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(TaskState::default()),
            revision: AtomicUsize::new(0),
        }
    }

    /// Apply one update, returning the rendered ledger text after the update (empty ledger returns `None`).
    ///
    /// Validation failure (more than one step `in_progress` at a time) returns `Err`, with **no side effects**
    /// -- validation happens before the write, avoiding a half-updated dirty ledger.
    pub fn apply(&self, update: TodoUpdate) -> Result<Option<String>, ToolError> {
        if let Some(steps) = &update.steps {
            let in_progress = steps
                .iter()
                .filter(|step| step.status == TodoStatus::InProgress)
                .count();
            if in_progress > 1 {
                return Err(ToolError::ArgumentsError(
                    "at most one step may be in_progress at a time".into(),
                ));
            }
        }
        {
            let mut state = self.inner.lock().expect("todo lock poisoned");
            let before = state.clone();
            if update.clear == Some(true) {
                *state = TaskState::default();
            }
            if let Some(goal) = update.goal {
                let trimmed = goal.trim();
                state.goal = (!trimmed.is_empty()).then(|| trimmed.to_string());
            }
            if let Some(steps) = update.steps {
                state.steps = steps
                    .into_iter()
                    .map(|step| Step {
                        text: step.text,
                        status: step.status,
                    })
                    .collect();
            }
            if let Some(findings) = update.add_findings {
                append_unique(&mut state.findings, findings);
            }
            if let Some(questions) = update.add_open_questions {
                append_unique(&mut state.open_questions, questions);
            }
            if *state != before {
                self.revision.fetch_add(1, Ordering::Release);
            }
        }
        Ok(self.render())
    }

    /// Render the ledger into the injected text; an empty ledger returns `None` (no empty content is injected).
    ///
    /// Wrapped in XML tags (`plan.md`: the tag form improves the model's attention to structured information).
    pub fn render(&self) -> Option<String> {
        let state = self.inner.lock().expect("todo lock poisoned");
        let mut body = String::new();
        if let Some(goal) = &state.goal {
            let _ = writeln!(body, "<goal>{}</goal>", escape(goal));
        }
        if !state.steps.is_empty() {
            body.push_str("<steps>\n");
            for step in &state.steps {
                let mark = match step.status {
                    TodoStatus::Pending => "[ ]",
                    TodoStatus::InProgress => "[>]",
                    TodoStatus::Completed => "[x]",
                };
                let _ = writeln!(body, "- {} {}", mark, escape(&step.text));
            }
            body.push_str("</steps>\n");
        }
        if !state.findings.is_empty() {
            body.push_str("<findings>\n");
            for finding in &state.findings {
                let _ = writeln!(body, "- {}", escape(finding));
            }
            body.push_str("</findings>\n");
        }
        if !state.open_questions.is_empty() {
            body.push_str("<open_questions>\n");
            for question in &state.open_questions {
                let _ = writeln!(body, "- {}", escape(question));
            }
            body.push_str("</open_questions>\n");
        }
        if body.is_empty() {
            return None;
        }
        let mut rendered = format!("<task_state>\n{body}</task_state>");
        if rendered.chars().count() > MAX_RENDER_CHARS {
            rendered = rendered.chars().take(MAX_RENDER_CHARS).collect();
            rendered.push_str("\n... (task ledger truncated; keep it concise)");
        }
        Some(rendered)
    }

    /// Structured read-only view (for UI rendering of task progress). An empty ledger (no goal and no steps) returns `None`.
    ///
    /// findings / `open_questions` do not enter the view: the UI task card only cares about "goal + step progress";
    /// conclusions and open questions are already conveyed by the model in the body text, and piling them into the status line only dilutes the signal.
    pub fn view(&self) -> Option<TaskView> {
        let state = self.inner.lock().expect("todo lock poisoned");
        if state.goal.is_none() && state.steps.is_empty() {
            return None;
        }
        let steps: Vec<TaskStepView> = state
            .steps
            .iter()
            .map(|step| TaskStepView {
                text: step.text.clone(),
                status: step.status,
            })
            .collect();
        let completed = steps
            .iter()
            .filter(|step| step.status == TodoStatus::Completed)
            .count();
        let current = steps
            .iter()
            .find(|step| step.status == TodoStatus::InProgress)
            .map(|step| step.text.clone());
        Some(TaskView {
            goal: state.goal.clone(),
            total: steps.len(),
            completed,
            current,
            steps,
        })
    }
}

fn append_unique(target: &mut Vec<String>, values: Vec<String>) {
    for value in values {
        let value = value.trim();
        if !value.is_empty() && !target.iter().any(|existing| existing == value) {
            target.push(value.to_string());
        }
    }
}

impl Default for TodoStore {
    fn default() -> Self {
        Self::new()
    }
}

/// A one-line description added to the ledger when injecting the request (so the model knows what this is and what to do).
///
/// Separate from [`TodoStore::render`]: the tool result does not need this description repeated.
pub const TASK_STATE_HEADER: &str = include_str!("../prompts/todo_header.md");

/// The nudge reminder (nag) injected when the model has not updated the ledger for several rounds.
///
/// Separate from `TASK_STATE_HEADER`: this one appears only when the ledger is **empty** or has **gone stale**,
/// as a "cold-start guardrail" -- when the ledger is empty there is no header to inject, so the model lacks an explicit
/// trigger instruction at the start; this fills it in.
pub const TODO_NAG_REMINDER: &str = include_str!("../prompts/todo_nag.md");

/// XML text escaping: prevents the model's `<` / `&` from breaking the tag structure.
fn escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Provider that injects the task ledger at the end of every request.
///
/// A non-empty ledger is injected every round; an empty ledger, or one that has had no valid update for several rounds, triggers a reminder.
/// Summary snapshots only read the ledger and do not affect the normal request count.
pub struct TodoContextProvider {
    store: Arc<TodoStore>,
    /// The last observed revision and the number of normal requests since the last valid update.
    progress: Mutex<(usize, usize)>,
}

impl TodoContextProvider {
    pub fn new(store: Arc<TodoStore>) -> Self {
        Self {
            store,
            progress: Mutex::new((0, 0)),
        }
    }
}

impl ContextProvider for TodoContextProvider {
    fn context(&self) -> Option<String> {
        let revision = self.store.revision.load(Ordering::Acquire);
        let nag = {
            let mut progress = self.progress.lock().expect("todo progress lock poisoned");
            if progress.0 == revision {
                progress.1 = progress.1.saturating_add(1);
            } else {
                *progress = (revision, 0);
            }
            progress.1 >= TODO_NAG_AFTER_ROUNDS
        };
        let mut text = self.compaction_context().unwrap_or_default();
        if nag {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(TODO_NAG_REMINDER);
        }
        (!text.is_empty()).then_some(text)
    }

    fn compaction_context(&self) -> Option<String> {
        self.store
            .render()
            .map(|ledger| format!("{TASK_STATE_HEADER}\n\n{ledger}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_then_render_roundtrip() {
        let store = TodoStore::new();
        assert!(store.render().is_none());
        let rendered = store
            .apply(TodoUpdate {
                goal: Some("implement the todo tool".into()),
                steps: Some(vec![
                    TodoStep {
                        text: "build the module".into(),
                        status: TodoStatus::Completed,
                    },
                    TodoStep {
                        text: "wire up the runtime".into(),
                        status: TodoStatus::Pending,
                    },
                ]),
                ..Default::default()
            })
            .unwrap()
            .expect("a non-empty ledger should render");
        assert!(rendered.contains("<goal>implement the todo tool</goal>"));
        assert!(rendered.contains("- [x] build the module"));
        assert!(rendered.contains("- [ ] wire up the runtime"));
    }

    #[test]
    fn patch_only_touches_provided_fields() {
        let store = TodoStore::new();
        store
            .apply(TodoUpdate {
                goal: Some("goal".into()),
                ..Default::default()
            })
            .unwrap();
        store
            .apply(TodoUpdate {
                add_findings: Some(vec!["finding A".into()]),
                ..Default::default()
            })
            .unwrap();
        let rendered = store.render().unwrap();
        assert!(
            rendered.contains("<goal>goal</goal>"),
            "goal should be preserved"
        );
        assert!(rendered.contains("- finding A"));
    }

    #[test]
    fn clear_resets_everything() {
        let store = TodoStore::new();
        store
            .apply(TodoUpdate {
                goal: Some("goal".into()),
                ..Default::default()
            })
            .unwrap();
        store
            .apply(TodoUpdate {
                clear: Some(true),
                ..Default::default()
            })
            .unwrap();
        assert!(store.render().is_none());
    }

    #[test]
    fn rejects_multiple_in_progress() {
        let store = TodoStore::new();
        let err = store
            .apply(TodoUpdate {
                steps: Some(vec![
                    TodoStep {
                        text: "a".into(),
                        status: TodoStatus::InProgress,
                    },
                    TodoStep {
                        text: "b".into(),
                        status: TodoStatus::InProgress,
                    },
                ]),
                ..Default::default()
            })
            .unwrap_err();
        assert!(matches!(err, ToolError::ArgumentsError(_)));
        assert!(
            store.render().is_none(),
            "validation failure should not produce side effects"
        );
    }

    #[test]
    fn escape_protects_tag_structure() {
        let store = TodoStore::new();
        let rendered = store
            .apply(TodoUpdate {
                goal: Some("a < b & c > d".into()),
                ..Default::default()
            })
            .unwrap()
            .unwrap();
        assert!(rendered.contains("a &lt; b &amp; c &gt; d"));
        assert!(!rendered.contains("a < b"));
    }

    #[test]
    fn provider_injects_ledger_when_non_empty() {
        let store = Arc::new(TodoStore::new());
        let provider = TodoContextProvider::new(Arc::clone(&store));
        assert!(
            provider.context().is_none(),
            "an empty ledger is not injected in the first round"
        );
        store
            .apply(TodoUpdate {
                goal: Some("do something".into()),
                ..Default::default()
            })
            .unwrap();
        let text = provider
            .context()
            .expect("a non-empty ledger should be injected");
        assert!(text.contains(TASK_STATE_HEADER));
        assert!(text.contains("<goal>do something</goal>"));
    }

    #[test]
    fn provider_nags_after_threshold_when_empty() {
        let store = Arc::new(TodoStore::new());
        let provider = TodoContextProvider::new(store);
        // No injection for the first few rounds; nagging starts after the threshold.
        let mut nagged = false;
        for _ in 0..=TODO_NAG_AFTER_ROUNDS {
            if let Some(text) = provider.context() {
                assert!(text.contains("<reminder>"));
                nagged = true;
                break;
            }
        }
        assert!(nagged, "an empty ledger left stale should trigger a nag");
    }

    #[test]
    fn nonempty_stale_ledger_nags_and_only_new_evidence_resets_it() {
        let store = Arc::new(TodoStore::new());
        store
            .apply(TodoUpdate {
                goal: Some("fix session switching".into()),
                add_findings: Some(vec!["active_agent is empty while running".into()]),
                ..Default::default()
            })
            .unwrap();
        let provider = TodoContextProvider::new(Arc::clone(&store));
        assert!(!provider.context().unwrap().contains("<reminder>"));
        for _ in 0..TODO_NAG_AFTER_ROUNDS {
            // Summary snapshots should not advance/reset the normal request count.
            assert!(
                !provider
                    .compaction_context()
                    .unwrap()
                    .contains("<reminder>")
            );
            provider.context();
        }
        assert!(provider.context().unwrap().contains("<reminder>"));
        store
            .apply(TodoUpdate {
                add_findings: Some(vec![" active_agent is empty while running ".into()]),
                ..Default::default()
            })
            .unwrap();
        assert!(
            provider.context().unwrap().contains("<reminder>"),
            "a duplicate conclusion does not count as progress"
        );
        assert_eq!(
            store
                .render()
                .unwrap()
                .matches("active_agent is empty while running")
                .count(),
            1
        );
        store
            .apply(TodoUpdate {
                add_findings: Some(vec!["modelName returns an empty string".into()]),
                ..Default::default()
            })
            .unwrap();
        assert!(!provider.context().unwrap().contains("<reminder>"));
    }
}
