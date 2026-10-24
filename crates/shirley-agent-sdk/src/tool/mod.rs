use std::any::{Any, TypeId};
use std::sync::Arc;
use std::{collections::HashMap, pin::Pin};

use serde::Serialize;

use crate::error::{ErrorKind, SdkError};
use crate::message;

/// Tool name reserved by the protocol layer: in content mode, when parsing finds an
/// incomplete / invalid tool block, `parse_content_response` synthesizes a call to this;
/// the runtime can feed a "correctable echo-back" back into the main loop without any
/// change (see `adapter/tool_calling.rs` Step 5).
pub(crate) const PROTOCOL_ERROR_TOOL: &str = "__protocol__";

/// Tool-layer error.
///
/// The display format is uniformly `[prefix]: detail`, consistent with `AdapterError` /
/// `SandboxError` / `WorkspaceError`; the mechanism is uniformly thiserror-derived, no
/// hand-written `Display`.
///
/// The prefix deliberately lives next to the variant rather than being derived from
/// [`ErrorKind`]: `ErrorKind` is the coarse retry-decision axis, and several variants map
/// to the same kind (e.g. `NotFoundError` and `ArgumentsError` here are both `BadRequest`).
/// The model needs the prefix to tell "the tool doesn't exist" from "the arguments are
/// wrong" — the latter it can fix itself by changing the arguments.
#[derive(Debug, Serialize, thiserror::Error)]
pub enum ToolError {
    #[error("[execution error]: {0}")]
    ExecutionError(String),

    #[error("[duplicate tool]: {0}")]
    RepetitionError(String),

    #[error("[tool not found]: {0}")]
    NotFoundError(String),

    #[error("[invalid arguments]: {0}")]
    ArgumentsError(String),
}

impl SdkError for ToolError {
    fn kind(&self) -> ErrorKind {
        match self {
            // The tool itself failed; the model should inspect the details and change strategy.
            Self::ExecutionError(_) => ErrorKind::ToolFailure,
            // Tool missing / invalid arguments / duplicate registration: the caller's (or the
            // model's) request is at fault.
            Self::RepetitionError(_) | Self::NotFoundError(_) | Self::ArgumentsError(_) => {
                ErrorKind::BadRequest
            }
        }
    }
}

pub type ToolName = String;

/// The result of a tool execution.
///
/// The error type is [`ToolError`], not [`crate::AgentError`]: the tool layer only owns
/// "did this run succeed or not"; which layer to attribute it to and whether to retry is
/// the runtime's business.
/// This matches the sandbox backend — `ProcessBackend::execute` only returns `SandboxError`.
pub type ToolFuture<'a> =
    Pin<Box<dyn Future<Output = Result<serde_json::Value, ToolError>> + Send + 'a>>;

/// The tool runtime context.
///
/// Tools run **concurrently** inside the `ReAct` loop (`runtime/agent.rs` uses
/// `FuturesUnordered`), so the context can't be `&mut Session` — several tools running at
/// once and all wanting a mutable borrow of the same session would necessarily conflict.
/// The only workable shape is an **interior-mutability handle** (`Arc<Mutex<Session>>` and
/// the like).
///
/// This type is a type-erased container: the application puts arbitrary
/// `Send + Sync + 'static` data in with [`ToolContext::with`], and a tool takes it back
/// out with [`ToolContext::get`]. What's stored is an `Arc`, so clone only bumps the
/// refcount and per-call dispatch is cheap.
///
/// ```
/// use shirley_agent_sdk::ToolContext;
/// use std::sync::Mutex;
///
/// let ctx = ToolContext::new().with(Mutex::new(42_u32));
/// let value = ctx.get::<Mutex<u32>>().expect("session not provided");
/// assert_eq!(*value.lock().unwrap(), 42);
/// ```
#[derive(Clone, Default)]
pub struct ToolContext {
    inner: Arc<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The stored values are `dyn Any` and cannot be formatted; show the entry count.
        f.debug_struct("ToolContext")
            .field("entries", &self.inner.len())
            .finish()
    }
}

impl ToolContext {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Store a piece of application data (chainable). A later store of the same type wins.
    #[must_use]
    pub fn with<T: Any + Send + Sync>(mut self, value: T) -> Self {
        self.insert(value);
        self
    }

    /// Store a piece of application data in place. For contexts holding a
    /// `&mut ToolContext`, such as `Tool::on_register` (the chainable [`ToolContext::with`]
    /// consumes `self` and can't be used there).
    /// A later store of the same type wins.
    pub fn insert<T: Any + Send + Sync>(&mut self, value: T) {
        Arc::make_mut(&mut self.inner).insert(TypeId::of::<T>(), Arc::new(value));
    }

    /// Take out the application data. Returns `None` when the type doesn't match or was never injected.
    #[must_use]
    pub fn get<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.inner
            .get(&TypeId::of::<T>())
            .and_then(|value| Arc::clone(value).downcast::<T>().ok())
    }

    /// Remove a piece of application data, returning the removed value (`None` when absent
    /// or of the wrong type).
    ///
    /// For a tool's `on_unregister` (= destroy) to clean up the context item it injected.
    /// Internally this is `Arc<HashMap>` + [`Arc::make_mut`] copy-on-write: if an in-flight
    /// tool call still holds an old clone, `make_mut` copies first and then mutates, so
    /// **in-flight calls still see the old data** (safe, no dangling), while new calls no
    /// longer see the removed item.
    ///
    /// Note it is indexed by [`TypeId`]: if two tools inject the **same type**, unregistering
    /// one clears the other's data too. The convention is for each tool to carry its state in
    /// its own newtype; a handle shared across tools should not be removed by a single tool in
    /// `on_unregister`.
    pub fn remove<T: Any + Send + Sync>(&mut self) -> Option<Arc<T>> {
        Arc::make_mut(&mut self.inner)
            .remove(&TypeId::of::<T>())
            .and_then(|value| value.downcast::<T>().ok())
    }
}

#[derive(Debug)]
pub struct ToolDefinition {
    pub name: String,

    pub description: String,

    pub parameters: serde_json::Value,
}

pub trait Tool: Send + Sync {
    // Get the name, description, and parameter schema
    fn definition(&self) -> &ToolDefinition;

    // Called internally by the SDK with JSON; after a tool call is recognized, it is
    // deserialized into the corresponding Arguments type.
    // ctx is the call-level runtime context, see [`ToolContext`].
    fn invoke(&self, input: serde_json::Value, ctx: ToolContext) -> ToolFuture<'_>;

    /// Register (= created). [`ToolManager::register`] calls this once after the duplicate
    /// check passes and before insertion.
    ///
    /// Here a tool can (a) write shared runtime dependencies into the context / store them in
    /// `self`; (b) self-check its config, returning `Err` when something is missing —
    /// **registration fails, the tool does not enter the table**, leaving no half-registered
    /// state.
    ///
    /// Defaults to a no-op: stateless tools need no change. Synchronous (registration happens
    /// at construction time; no async introduced).
    ///
    /// # Errors
    ///
    /// Returns [`ToolError`] to abort registration; the tool then does not enter the table.
    fn on_register(&mut self, _ctx: &mut ToolContext) -> Result<(), ToolError> {
        Ok(())
    }

    /// Unregister (= destroy). [`ToolManager::unregister`] calls this once **after** removing
    /// the tool from the table.
    ///
    /// Here a tool releases the context item it injected (e.g. `ctx.remove::<T>()`).
    ///
    /// Semantics: **best-effort cleanup**. Whether it returns `Ok` or `Err`, the tool has
    /// already been removed from the table; `Err` only tells the caller "cleanup failed" and
    /// does not resurrect the tool.
    /// Defaults to a no-op: stateless tools need no change.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError`] to signal cleanup failure; the tool is already removed from the table.
    fn on_unregister(&mut self, _ctx: &mut ToolContext) -> Result<(), ToolError> {
        Ok(())
    }
}
pub struct ToolManager {
    tools: HashMap<ToolName, Box<dyn Tool>>,
    /// The tool runtime context. Owned by the manager (no longer held / passed separately by
    /// the application layer): tools write dependencies into it in `on_register`, the runtime
    /// clones it for dispatch on each call, and `on_unregister` cleans up.
    /// See `docs/tool-lifecycle.md`.
    context: ToolContext,
}

impl std::fmt::Debug for ToolManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Tools are `Box<dyn Tool>` (no `Debug`); show their names instead.
        f.debug_struct("ToolManager")
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .field("context", &self.context)
            .finish()
    }
}

impl Default for ToolManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            context: ToolContext::new(),
        }
    }

    #[must_use]
    pub fn definitions(&self) -> Vec<&ToolDefinition> {
        let mut definitions = self
            .tools
            .values()
            .map(|tool| tool.definition())
            .collect::<Vec<&ToolDefinition>>();

        definitions.sort_by(|a, b| a.name.cmp(&b.name));
        definitions
    }

    /// Register a tool, running its `on_register` hook before insertion.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::RepetitionError`] when a tool with the same name is already
    /// registered, or a [`ToolError`] from the tool's `on_register` hook.
    pub fn register(&mut self, mut tool: impl Tool + 'static) -> Result<(), ToolError> {
        // 1. Check whether the tool is a duplicate; throw if so
        let tool_name = tool.definition().name.clone();
        if self.tools.contains_key(&tool_name) {
            return Err(ToolError::RepetitionError(format!(
                "tool already registered: {tool_name}"
            )));
        }

        // 2. created: after the duplicate check passes and before insertion, run the
        //    register hook. On failure the tool does not enter the table — no "half
        //    initialized" tool is left behind (see `docs/tool-lifecycle.md` 4.3).
        tool.on_register(&mut self.context)?;

        // 3. Not a duplicate and initialized successfully; add the tool to self.tools
        self.tools.insert(tool_name, Box::new(tool));

        Ok(())
    }

    /// Remove a registered tool (= the destroy trigger path).
    ///
    /// Order: **remove from the table first → then call `on_unregister`**. Even if the
    /// callback errors, the tool does not come back, so there is no inconsistent state of
    /// "destroy was called but it's still in the table". Returns [`ToolError::NotFoundError`]
    /// when the name isn't registered.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::NotFoundError`] when the name isn't registered, or a
    /// [`ToolError`] from the tool's `on_unregister` hook.
    pub fn unregister(&mut self, name: &str) -> Result<(), ToolError> {
        let mut tool = self
            .tools
            .remove(name)
            .ok_or_else(|| ToolError::NotFoundError(name.to_string()))?;

        tool.on_unregister(&mut self.context)
    }

    /// A read-only view of the tool runtime context (the runtime clones it for dispatch before a call).
    #[must_use]
    pub fn context(&self) -> &ToolContext {
        &self.context
    }

    /// A mutable view of the tool runtime context: the application injects handles shared
    /// across tools here before/after registration (per-tool private state is better placed in
    /// its `on_register`; see `docs/tool-lifecycle.md` 4.4).
    pub fn context_mut(&mut self) -> &mut ToolContext {
        &mut self.context
    }

    /// Invoke a tool.
    ///
    /// Returns `ToolError`: if the caller (the runtime) needs `AgentError`, it can converge
    /// automatically with `?` rather than wrapping here prematurely.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError`] when the tool is not found, its arguments are invalid,
    /// or the tool's execution fails.
    pub async fn invoke(
        &self,
        input: &message::ToolCall,
        ctx: ToolContext,
    ) -> Result<serde_json::Value, ToolError> {
        // Protocol-reserved tool: don't look it up; return the `message` argument directly as
        // the echo-back content. This lets a "parse failure" travel back to the model through
        // the tool-result channel with zero changes to agent.rs.
        if input.name == PROTOCOL_ERROR_TOOL {
            let message = serde_json::from_str::<serde_json::Value>(&input.arguments)
                .ok()
                .and_then(|value| {
                    value
                        .get("message")
                        .and_then(|m| m.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| {
                    "Malformed tool call; please resend a complete, valid call.".to_string()
                });
            return Ok(serde_json::Value::String(message));
        }

        let tool = self
            .tools
            .get(&input.name)
            .ok_or_else(|| ToolError::NotFoundError(input.name.clone()))?;

        let arguments = serde_json::from_str(&input.arguments).map_err(|error| {
            ToolError::ArgumentsError(format!("arguments is not valid JSON: {error}"))
        })?;

        // Hand off to the tool to process its own arguments
        tool.invoke(arguments, ctx).await
    }
}
