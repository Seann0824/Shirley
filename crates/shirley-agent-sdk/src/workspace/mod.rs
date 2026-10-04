use std::{
    fs,
    io,
    path::{Component, Path, PathBuf},
};

use crate::error::{ErrorKind, SdkError};

pub struct WorkSpace {
    root: PathBuf,
}

/// 工作区路径校验失败。
///
/// 刻意区分 `OutsideRoot` 与 `InvalidPath`：前者是"越界被拒绝"，
/// 后者是"这个路径本身没法用"，模型需要靠这个差别决定是换路径还是换写法。
///
/// 展示格式统一为 `[前缀]: 详情`，与 `AdapterError` / `ToolError` /
/// `SandboxError` 一致；前缀留在变体旁，不从 [`ErrorKind`] 推导
/// （两者同属 `BadRequest`，但语义不同）。
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    /// 请求的路径落在工作区根目录之外。
    #[error("[path outside workspace]: {0}")]
    OutsideRoot(String),

    /// 路径本身不合法（无法解析、指向符号链接等）。
    #[error("[invalid path]: {0}")]
    InvalidPath(String),

    /// 底层文件系统错误。
    #[error("[workspace io error]: {0}")]
    Io(#[source] io::Error),
}

impl SdkError for WorkspaceError {
    fn kind(&self) -> ErrorKind {
        match self {
            // 越界与非法路径都是请求侧问题：重试同一份输入不会变好。
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

    // reoslve 解析路径
    pub fn resolve(&self, input: &str) -> Result<PathBuf, WorkspaceError> {
        let input_path = Path::new(input);

        if input_path.is_absolute() {
            return Err(WorkspaceError::OutsideRoot(input.to_owned()));
        }
        // 按照 . 和 .. 展开，不允许超过 workspace root
        let mut relative = PathBuf::new();
        for component in input_path.components() {
            match component {
                // "." 表示当前目录，不改变路径，所以忽略。
                Component::CurDir => {}
                // 普通目录名或文件名，追加到相对路径末尾。
                Component::Normal(part) => relative.push(part),
                // ".." 表示退回上一级。
                Component::ParentDir => {
                    // 如果已经没有路径组件可退，说明试图越过 workspace 根目录。
                    if !relative.pop() {
                        return Err(WorkspaceError::OutsideRoot(input.to_owned()));
                    }
                }
                // 根目录或盘符前缀意味着路径带有绝对路径特征，拒绝。
                Component::RootDir | Component::Prefix(_) => {
                    return Err(WorkspaceError::OutsideRoot(input.to_owned()));
                }
            }
        }
        let candidate = self.root.join(relative);

        // 已存在时规范化完整路径，可检测逃出 root 的符号链接。
        match fs::canonicalize(&candidate) {
            Ok(canonical) => {
                self.ensure_within_root(&canonical, input)?;
                Ok(canonical)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // canonicalize 对 dangling symlink 也会报 NotFound，不能把它当普通新文件。
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
