//! Resumable host-side filesystem boundaries. Native child denials remain
//! non-resumable unless their OS backend can suspend the original operation.
use super::{Access, AccessDenied, DenialRecord, RuntimeSandboxPolicy};
use crate::{app::AppToolExecutionContext, file_io};
use async_trait::async_trait;
use miette::Result;
use std::path::Path;

#[async_trait]
pub trait SandboxAccessBroker: Send + Sync {
    async fn authorize(&self, denial: DenialRecord) -> Result<RuntimeSandboxPolicy>;
}

/// Domain I/O uses the common broker at the actual filesystem boundary. No App
/// declares review requirements or predicts permission needs from a tool name.
pub struct FileSystem<'a> {
    context: &'a AppToolExecutionContext,
}

impl<'a> FileSystem<'a> {
    pub fn new(context: &'a AppToolExecutionContext) -> Self {
        Self { context }
    }

    async fn access(
        &self,
        policy: &RuntimeSandboxPolicy,
        path: &Path,
        access: Access,
    ) -> Result<RuntimeSandboxPolicy> {
        let check = match access {
            Access::Read => file_io::ensure_authorized_read(path, policy, "filesystem access"),
            Access::Write => policy.ensure_path_writable(path, "filesystem access"),
        };
        match check {
            Ok(()) => Ok(policy.clone()),
            Err(error) => {
                let Some(denial) = error.downcast_ref::<AccessDenied>() else {
                    return Err(error);
                };
                let Some(broker) = &self.context.access_broker else {
                    return Err(error);
                };
                let granted = broker.authorize(denial.record.clone()).await?;
                match access {
                    Access::Read => file_io::ensure_authorized_read(
                        path,
                        &granted,
                        "resumed filesystem access",
                    )?,
                    Access::Write => {
                        granted.ensure_path_writable(path, "resumed filesystem access")?
                    }
                }
                Ok(granted)
            }
        }
    }

    async fn read_policy(&self, value: &str) -> Result<RuntimeSandboxPolicy> {
        let path = file_io::resolve_read_file_path(
            &self.context.execution_cwd,
            &self.context.sandbox_policy,
            value,
        )?;
        self.access(&self.context.sandbox_policy, &path, Access::Read)
            .await
    }

    async fn write_policy(
        &self,
        initial: &RuntimeSandboxPolicy,
        value: &str,
    ) -> Result<RuntimeSandboxPolicy> {
        let path = file_io::resolve_file_path(&self.context.execution_cwd, value)?;
        // Atomic writes require a writable parent for temporary-file creation
        // and rename. The check belongs to the filesystem operation itself.
        let parent = path
            .parent()
            .ok_or_else(|| miette::miette!("File has no parent directory"))?;
        let policy = self.access(initial, parent, Access::Write).await?;
        self.access(&policy, &path, Access::Write).await
    }

    pub async fn read_file(&self, args: &file_io::ReadFileArgs) -> Result<file_io::ReadFileResult> {
        let policy = self.read_policy(&args.path).await?;
        file_io::read_file(&self.context.execution_cwd, &policy, args)
    }
    pub async fn list_directory(
        &self,
        args: &file_io::ListDirectoryArgs,
    ) -> Result<file_io::DirectoryPage> {
        let policy = self.read_policy(&args.path).await?;
        file_io::list_directory(&self.context.execution_cwd, &policy, args)
    }
    pub async fn search_text(
        &self,
        args: &file_io::SearchTextArgs,
    ) -> Result<file_io::SearchResult> {
        let policy = self.read_policy(&args.path).await?;
        file_io::search_text(&self.context.execution_cwd, &policy, args)
    }
    pub async fn write_file(
        &self,
        args: &file_io::WriteFileArgs,
    ) -> Result<file_io::FileWriteResult> {
        let policy = self
            .write_policy(&self.context.sandbox_policy, &args.path)
            .await?;
        file_io::write_file(&self.context.execution_cwd, &policy, args)
    }
    pub async fn edit_file(
        &self,
        args: &file_io::EditFileArgs,
    ) -> Result<file_io::FileWriteResult> {
        let policy = self
            .write_policy(&self.context.sandbox_policy, &args.path)
            .await?;
        file_io::edit_file(&self.context.execution_cwd, &policy, args)
    }
    pub async fn copy_file(
        &self,
        args: &file_io::CopyFileArgs,
    ) -> Result<file_io::FileWriteResult> {
        let read = self.read_policy(&args.source).await?;
        let write = self.write_policy(&read, &args.destination).await?;
        let source =
            file_io::resolve_read_file_path(&self.context.execution_cwd, &read, &args.source)?;
        // Both capabilities were obtained before any write. Re-check source
        // under the fresh shared policy before reading or committing bytes.
        file_io::ensure_authorized_read(&source, &write, "copy source")?;
        file_io::copy_file(&self.context.execution_cwd, &write, args)
    }
    pub async fn read_bytes(&self, path: &Path, limit: u64) -> Result<Vec<u8>> {
        let path = self.context.resolve_tool_path(path, None);
        let policy = self
            .access(&self.context.sandbox_policy, &path, Access::Read)
            .await?;
        file_io::read_bytes(&path, limit, &policy)
    }

    pub async fn ensure_directory(&self, path: &Path) -> Result<RuntimeSandboxPolicy> {
        let path = self.context.resolve_tool_path(path, None);
        let policy = if path.exists() {
            self.access(&self.context.sandbox_policy, &path, Access::Read)
                .await?
        } else {
            let mut first_missing = path.as_path();
            while let Some(parent) = first_missing.parent() {
                if parent.exists() {
                    break;
                }
                first_missing = parent;
            }
            self.access(&self.context.sandbox_policy, first_missing, Access::Write)
                .await?
        };
        file_io::ensure_directory(&policy, &path)?;
        Ok(policy)
    }
    pub async fn view_image(
        &self,
        image_dir: &Path,
        supports_vision: bool,
        args: &file_io::ViewImageArgs,
    ) -> Result<file_io::ViewedImage> {
        if !supports_vision {
            return Err(miette::miette!("The model does not support image inputs"));
        }
        let policy = self.read_policy(&args.path).await?;
        file_io::view_image(
            &self.context.execution_cwd,
            &policy,
            image_dir,
            supports_vision,
            args,
        )
    }
}
