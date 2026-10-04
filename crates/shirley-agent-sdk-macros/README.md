# shirley-agent-sdk-macros

Procedural macros for [`shirley-agent-sdk`](https://crates.io/crates/shirley-agent-sdk).

> **You should not depend on this crate directly.** The `#[tool]` macro is re-exported from the
> `shirley-agent-sdk` root, so add that crate instead — this one is pulled in automatically as a
> transitive dependency.

```toml
[dependencies]
shirley-agent-sdk = "0.0.1"
```

## The `#[tool]` macro

`#[tool]` turns a plain async function into a model-callable tool. It generates an `Arguments`
struct (with `Deserialize` + `JsonSchema` and `deny_unknown_fields`), a `GenerateTool` type
implementing `shirley_agent_sdk::Tool`, and a `tool()` constructor used for registration.

```rust
use shirley_agent_sdk::ToolError;
use shirley_agent_sdk::tool;   // re-exported from the SDK root

#[tool(description = "Execute a shell command")]
async fn bash(
    #[param(description = "Command to run")] command: String,
    #[param(description = "Timeout in seconds")] timeout: Option<u64>,
) -> Result<String, ToolError> {
    // ... run the command via the sandbox and return its output
    Ok(String::new())
}
```

Register it with a `ToolManager`:

```rust
use shirley_agent_sdk::ToolManager;

let mut tools = ToolManager::new();
tools.register(bash_tool::tool())?;
```

The macro expands into a module named after the function (here `bash_tool`), so the tool is
referenced as `bash_tool::tool()`.

### Rules and limitations

- The annotated function must not take `self`.
- Every parameter needs a `#[param(description = "...")]` attribute; omitting one is a compile error.
- Parameters cannot use destructuring patterns, `ref`, or `@` bindings.
- The function and the generated module must live at the same level — the generated code calls
  `super::<function_name>`, so wrapping the function in an inner module breaks it.
- The tool's error type is `shirley_agent_sdk::ToolError`; tool authors never need to reference
  `AgentError`.

## License

Licensed under either of MIT or Apache-2.0, at your option.
