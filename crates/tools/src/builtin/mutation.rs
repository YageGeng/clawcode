use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock, Weak};

use cap_fs_ext::{
    DirExt as _, FollowSymlinks, MetadataExt as _, OpenOptionsFollowExt as _,
};
use cap_std::fs::{Dir, File as CapFile, OpenOptions};
use protocol::ToolCall;
use tokio::fs::File;
use tokio::io::{AsyncSeekExt as _, AsyncWriteExt as _};
use tokio::sync::{Mutex, OwnedMutexGuard};

use super::path::ResolvedPath;
use crate::{ToolError, ToolExecutionContext};

/// A mutation's queue key and, for guarded saves, its pinned physical path.
pub(super) struct FileMutation {
    pub(super) path: PathBuf,
    guarded: bool,
}

impl FileMutation {
    /// Resolves the operation without creating anything, then obtains any external approval.
    pub(super) async fn prepare(
        input: &str,
        call: &ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<Self, ToolError> {
        context.ensure_active()?;
        let path = ResolvedPath::new(input, &context.cwd)?.as_path().to_owned();
        let approver = match &context.file_mutation_approver {
            Some(approver) if approver.requires_guarded_save(call)? => approver,
            // Hosts with in-place save semantics keep the original path spelling.
            _ => {
                return Ok(Self {
                    path,
                    guarded: false,
                });
            }
        };
        let cwd = context.cwd.clone();
        let can_create = call.name == "write";
        let (workspace, path) = tokio::task::spawn_blocking(move || {
            // Only write may create missing components. Edit must traverse the full existing path.
            let path = if can_create {
                Self::physical_path(&path)?
            } else {
                // Some platforms collapse parents before opening. Verify every traversed parent explicitly.
                for directory in path
                    .ancestors()
                    .filter(|part| part.ends_with(".."))
                    .filter_map(Path::parent)
                {
                    if !directory.metadata()?.is_dir() {
                        return Err(io::Error::new(
                            io::ErrorKind::NotADirectory,
                            "edit path traverses a non-directory",
                        ));
                    }
                }
                path.canonicalize()?
            };
            Ok((cwd.canonicalize()?, path))
        })
        .await
        .map_err(io::Error::other)
        .and_then(|result| result)
        .map_err(|error| {
            tracing::warn!(
                "could not resolve {} target {}: {}",
                call.name,
                input,
                error
            );
            ToolError::Execution {
                tool: call.name.clone(),
                message: format!(
                    "Could not resolve mutation target {input}: {error}"
                ),
            }
        })?;
        if !path.starts_with(&workspace) {
            approver.approve(call, &path).await?;
        }
        context.ensure_active()?;
        Ok(Self {
            path,
            guarded: true,
        })
    }

    /// Resolves links before parent components, including links that name a future file.
    fn physical_path(path: &Path) -> io::Result<PathBuf> {
        let mut pending = path.to_path_buf();
        'links: for _ in 0..=40 {
            // Components normalizes trailing separators and dots; neither can name a new file.
            let leaf = pending
                .as_os_str()
                .as_encoded_bytes()
                .rsplit(|byte| std::path::is_separator(char::from(*byte)))
                .next();
            if matches!(leaf, None | Some(b"" | b"." | b"..")) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "expected a file name",
                ));
            }
            match pending.canonicalize() {
                Ok(existing) => return Ok(existing),
                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                    return Err(error);
                }
                Err(_) => {}
            }
            let absolute = std::path::absolute(&pending)?;
            let mut resolved = PathBuf::new();
            let mut components = absolute.components();
            while let Some(component) = components.next() {
                match component {
                    Component::ParentDir => {
                        resolved.pop();
                    }
                    Component::CurDir => {}
                    Component::RootDir => {
                        resolved.push(component.as_os_str());
                        // Canonicalize the root spelling even for a missing child of a Windows drive.
                        resolved = resolved.canonicalize()?;
                    }
                    Component::Normal(name) => {
                        resolved.push(name);
                        match resolved.canonicalize() {
                            Ok(existing) => {
                                resolved = existing;
                                continue;
                            }
                            Err(error)
                                if error.kind() == io::ErrorKind::NotFound => {}
                            Err(error) => return Err(error),
                        }
                        // Missing suffixes are legal. A dangling link must first expand to its destination.
                        let link = match resolved.read_link() {
                            Ok(link) => link,
                            Err(error)
                                if error.kind() == io::ErrorKind::NotFound =>
                            {
                                continue;
                            }
                            Err(error) => return Err(error),
                        };
                        resolved.pop();
                        pending = resolved.join(link);
                        // Joining an empty suffix would turn an ordinary file destination into a directory.
                        let suffix = components.as_path();
                        if !suffix.as_os_str().is_empty() {
                            pending.push(suffix);
                        }
                        continue 'links;
                    }
                    _ => resolved.push(component.as_os_str()),
                }
            }
            return Ok(resolved);
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too many symbolic links",
        ))
    }

    /// Opens an existing file for editing and retains the descriptor that identifies its snapshot.
    pub(super) async fn open_edit(&self) -> io::Result<File> {
        if self.guarded {
            let path = self.path.clone();
            let file = tokio::task::spawn_blocking(move || {
                GuardedEntry::open(&path, false)?.open_file(true)
            })
            .await??;
            return Ok(File::from_std(file.into_std()));
        }
        tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)
            .await
    }

    /// Saves a complete document, using the edit snapshot when the caller already read the file.
    pub(super) async fn save(
        &self,
        original: Option<File>,
        content: String,
    ) -> io::Result<()> {
        if self.guarded {
            let path = self.path.clone();
            let original = match original {
                Some(file) => {
                    Some(cap_std::fs::File::from_std(file.into_std().await))
                }
                None => None,
            };
            return tokio::task::spawn_blocking(move || {
                GuardedEntry::open(&path, original.is_none())?
                    .save(original, &content)
            })
            .await?;
        }

        if let Some(mut file) = original {
            file.rewind().await?;
            file.set_len(0).await?;
            file.write_all(content.as_bytes()).await?;
            return file.flush().await;
        }
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&self.path, content).await
    }

    /// Serializes mutations that resolve to the same real file while allowing other files in parallel.
    pub(super) async fn lock(&self) -> OwnedMutexGuard<()> {
        let key = if self.guarded {
            self.path.clone()
        } else {
            tokio::fs::canonicalize(&self.path)
                .await
                .unwrap_or_else(|_| self.path.clone())
        };
        let queue = {
            let mut queues = FILE_MUTATION_QUEUES.lock().await;
            if let Some(queue) = queues.get(&key).and_then(Weak::upgrade) {
                queue
            } else {
                let queue = Arc::new(Mutex::new(()));
                queues.insert(key, Arc::downgrade(&queue));
                queue
            }
        };
        queue.lock_owned().await
    }
}

/// A resolved file name relative to its pinned parent directory.
struct GuardedEntry<'a> {
    dir: Dir,
    name: &'a OsStr,
}

impl<'a> GuardedEntry<'a> {
    /// Walks the approved parent by directory handles, refusing links installed after resolution.
    fn open(path: &'a Path, create: bool) -> io::Result<Self> {
        let root = path
            .ancestors()
            .last()
            .filter(|root| root.is_absolute())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "expected an absolute mutation target",
                )
            })?;
        let mut dir =
            Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
        let mut components = path
            .strip_prefix(root)
            .map_err(io::Error::other)?
            .components();
        let Some(Component::Normal(name)) = components.next_back() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected a file name",
            ));
        };
        for component in components {
            let Component::Normal(child) = component else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "expected a canonical mutation target",
                ));
            };
            dir = match dir.open_dir_nofollow(child) {
                Ok(child) => child,
                Err(error)
                    if create && error.kind() == io::ErrorKind::NotFound =>
                {
                    // Sibling writes may create the same directory. Reopen it without following links.
                    match dir.create_dir(child) {
                        Err(error)
                            if error.kind() != io::ErrorKind::AlreadyExists =>
                        {
                            return Err(error);
                        }
                        _ => {}
                    }
                    dir.open_dir_nofollow(child)?
                }
                Err(error) => return Err(error),
            };
        }
        Ok(Self { dir, name })
    }

    /// Checks the named file's write permissions without truncating it or following a new link.
    fn open_file(&self, read: bool) -> io::Result<CapFile> {
        let mut options = OpenOptions::new();
        options.read(read).write(true).follow(FollowSymlinks::No);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt as _;
            // Opening a FIFO for writing must never wait for a reader in the blocking pool.
            options.custom_flags(nix::libc::O_NONBLOCK);
        }
        let file = self.dir.open_with(self.name, &options)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected a regular file",
            ));
        }
        Ok(file)
    }

    /// Saves only the pinned name and verifies that an edit still refers to the file it read.
    fn save(&self, original: Option<CapFile>, content: &str) -> io::Result<()> {
        let lookup = || match self.open_file(false) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        };
        let identity = |file: &CapFile| {
            file.metadata()
                .map(|metadata| (metadata.dev(), metadata.ino()))
        };
        let current = lookup()?;
        let expected = current.as_ref().map(identity).transpose()?;
        if let Some(original) = original
            && expected != Some(identity(&original)?)
        {
            return Err(io::Error::other(
                "edit target changed while preparing the replacement",
            ));
        }

        #[cfg(not(target_os = "linux"))]
        if let Some(mut file) = current {
            // Native ACLs stay on the original inode. Refuse files known to have other names.
            if file.metadata()?.nlink() != 1 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "cannot safely replace a shared file on this platform",
                ));
            }
            file.set_len(0)?;
            file.write_all(content.as_bytes())?;
            return file.flush();
        }

        // Stage the full contents in the pinned directory. A new link to the original cannot see the write.
        let mut replacement = cap_tempfile::TempFile::new(&self.dir)?;
        #[cfg(target_os = "linux")]
        if let Some(source) = &current {
            self.preserve_access(source, replacement.as_file())?;
        }
        replacement.write_all(content.as_bytes())?;
        replacement.flush()?;
        if lookup()?.as_ref().map(identity).transpose()? != expected {
            return Err(io::Error::other(
                "mutation target changed before publication",
            ));
        }
        replacement.replace(self.name)
    }

    /// Preserves Linux ownership, access ACLs and user attributes before exposing any contents.
    #[cfg(target_os = "linux")]
    fn preserve_access(
        &self,
        source: &CapFile,
        replacement: &CapFile,
    ) -> io::Result<()> {
        use std::collections::BTreeSet;
        use std::os::unix::fs::{
            MetadataExt as _, PermissionsExt as _, fchown,
        };
        use xattr::FileExt as _;

        let source = source.try_clone()?.into_std();
        let replacement = replacement.try_clone()?.into_std();
        let original = source.metadata()?;
        let staged = replacement.metadata()?;
        if original.permissions().mode() & 0o6000 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cannot replace a set-user-ID or set-group-ID file",
            ));
        }
        let same_owners =
            (original.uid(), original.gid()) == (staged.uid(), staged.gid());
        // A named fallback may already have open readers. Never stage private bytes into a file
        // created with broader access, even if its permissions could be tightened afterwards.
        if cap_fs_ext::MetadataExt::nlink(&staged) != 0
            && (!same_owners
                || staged.mode() & !original.mode() & 0o777 != 0
                || source.get_xattr("system.posix_acl_access")?
                    != replacement.get_xattr("system.posix_acl_access")?)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "filesystem cannot stage a replacement with matching access restrictions",
            ));
        }
        if !same_owners {
            fchown(&replacement, Some(original.uid()), Some(original.gid()))?;
        }
        replacement.set_permissions(original.permissions())?;
        // Reconcile both sides so a directory's default ACL cannot grant new access to the file.
        let names: BTreeSet<_> = source
            .list_xattr()?
            .chain(replacement.list_xattr()?)
            .collect();
        for name in names {
            let key = name.as_encoded_bytes();
            if matches!(
                key,
                b"security.capability" | b"security.ima" | b"security.evm"
            ) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "cannot replace a file with capability or integrity attributes",
                ));
            }
            let value = source.get_xattr(&name)?;
            if value == replacement.get_xattr(&name)? {
                continue;
            }
            if !key.starts_with(b"user.") && key != b"system.posix_acl_access" {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "cannot preserve attribute {name:?} on {:?}",
                        self.name
                    ),
                ));
            }
            match value {
                Some(bytes) => replacement.set_xattr(&name, &bytes)?,
                None => replacement.remove_xattr(&name)?,
            }
        }
        Ok(())
    }
}

static FILE_MUTATION_QUEUES: LazyLock<
    Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));
