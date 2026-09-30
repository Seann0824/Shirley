use std::{
    fs,
    path::{Component, Path, PathBuf},
    todo,
};

use bon::builder;
use futures::{future::err, io};

pub struct WorkSpace {
    root: PathBuf,
}

pub enum WorkspaceError {
    // 路径不再工具区域
    OutsideRoot(String),
    InvalidPath(String),
    Io(io::Error),
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
