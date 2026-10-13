//! Workspace path confinement: `WorkSpace::resolve` maps a relative path under the root and
//! rejects anything that escapes it.
//!
//! The two error variants are deliberately different — `OutsideRoot` vs `InvalidPath` — so a
//! caller (typically a model) can tell "change the path" from "change the spelling".
//!
//! ```sh
//! cargo run -p shirley-agent-sdk --example workspace
//! ```

use std::path::PathBuf;

use shirley_agent_sdk::workspace::{WorkSpace, WorkspaceError};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The root must exist; canonicalize resolves symlinks and makes it absolute.
    let workspace = WorkSpace::new(PathBuf::from("."))?;

    let resolved = workspace.resolve("src/lib.rs")?;
    println!("src/lib.rs -> {}", resolved.display());

    // An absolute path and a `..` escape are both rejected.
    match workspace.resolve("../secrets.txt") {
        Err(WorkspaceError::OutsideRoot(path)) => println!("rejected escape: {path}"),
        other => println!("unexpected: {other:?}"),
    }
    match workspace.resolve("/etc/passwd") {
        Err(WorkspaceError::OutsideRoot(path)) => println!("rejected absolute: {path}"),
        other => println!("unexpected: {other:?}"),
    }
    Ok(())
}
