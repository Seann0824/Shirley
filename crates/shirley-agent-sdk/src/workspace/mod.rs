use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

use crate::error::{ErrorKind, SdkError};

#[derive(Debug)]
pub struct WorkSpace {
    root: PathBuf,
}

/// A workspace path-validation failure.
///
/// It deliberately distinguishes `OutsideRoot` from `InvalidPath`: the former is
/// "rejected for escaping the root", the latter is "this path is unusable as-is". The
/// model uses that difference to decide whether to change the path or change the spelling.
///
/// The display format is uniformly `[prefix]: detail`, consistent with `AdapterError` /
/// `ToolError` / `SandboxError`; the prefix lives next to the variant and is not derived
/// from [`ErrorKind`] (both map to `BadRequest`, but their semantics differ).
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    /// The requested path falls outside the workspace root.
    #[error("[path outside workspace]: {0}")]
    OutsideRoot(String),

    /// The path itself is invalid (unresolvable, points to a symlink, etc.).
    #[error("[invalid path]: {0}")]
    InvalidPath(String),

    /// An underlying filesystem error.
    #[error("[workspace io error]: {0}")]
    Io(#[source] io::Error),
}

impl SdkError for WorkspaceError {
    fn kind(&self) -> ErrorKind {
        match self {
            // Both escaping and invalid paths are request-side problems: retrying the same
            // input won't help.
            Self::OutsideRoot(_) | Self::InvalidPath(_) => ErrorKind::BadRequest,
            Self::Io(_) => ErrorKind::Internal,
        }
    }
}

impl From<io::Error> for WorkspaceError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl WorkSpace {
    pub fn new(root: PathBuf) -> Result<Self, WorkspaceError> {
        let root = fs::canonicalize(root)?;

        if !root.is_dir() {
            return Err(WorkspaceError::InvalidPath(root.display().to_string()));
        }
        Ok(Self { root })
    }

    // resolve a path against the workspace root
    pub fn resolve(&self, input: &str) -> Result<PathBuf, WorkspaceError> {
        let input_path = Path::new(input);

        if input_path.is_absolute() {
            return Err(WorkspaceError::OutsideRoot(input.to_owned()));
        }
        // Expand `.` and `..`, and never allow going above the workspace root
        let mut relative = PathBuf::new();
        for component in input_path.components() {
            match component {
                // "." is the current directory and doesn't change the path, so ignore it.
                Component::CurDir => {}
                // A normal directory or file name; append it to the relative path.
                Component::Normal(part) => relative.push(part),
                // ".." means going up one level.
                Component::ParentDir => {
                    // If there is nothing left to pop, it's trying to escape the workspace root.
                    if !relative.pop() {
                        return Err(WorkspaceError::OutsideRoot(input.to_owned()));
                    }
                }
                // A root dir or drive prefix means the path is absolute-like; reject it.
                Component::RootDir | Component::Prefix(_) => {
                    return Err(WorkspaceError::OutsideRoot(input.to_owned()));
                }
            }
        }
        let candidate = self.root.join(relative);

        // When it already exists, canonicalize the full path to detect symlinks escaping the root.
        match fs::canonicalize(&candidate) {
            Ok(canonical) => {
                self.ensure_within_root(&canonical, input)?;
                Ok(canonical)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // canonicalize also reports NotFound for a dangling symlink; don't treat it as a plain new file.
                match fs::symlink_metadata(&candidate) {
                    Ok(_) => {
                        return Err(WorkspaceError::InvalidPath(input.to_owned()));
                    }
                    Err(metadata_error) if metadata_error.kind() == io::ErrorKind::NotFound => {}
                    Err(metadata_error) => return Err(metadata_error.into()),
                }

                let parent = candidate.parent().unwrap_or(&self.root);
                let file_name = candidate
                    .file_name()
                    .ok_or_else(|| WorkspaceError::InvalidPath(input.to_owned()))?;

                let canonical_parent = fs::canonicalize(parent)?;
                self.ensure_within_root(&canonical_parent, input)?;

                Ok(canonical_parent.join(file_name))
            }
            Err(error) => Err(error.into()),
        }
    }

    fn ensure_within_root(&self, path: &Path, requested: &str) -> Result<(), WorkspaceError> {
        if path.starts_with(&self.root) {
            Ok(())
        } else {
            Err(WorkspaceError::OutsideRoot(requested.to_owned()))
        }
    }
}
