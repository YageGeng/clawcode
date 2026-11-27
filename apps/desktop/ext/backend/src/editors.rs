//! "Open in": the external editors installed on this computer, and opening a
//! local workspace in one of them or in the system file manager.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine as _;
use nsh_core::prelude::*;

use crate::apps::{AppLook, DesktopEntries};

/// The `openIn` value, and the command name, of the system file manager choice.
const FILE_MANAGER_CHOICE: &str = "file-manager";

/// The largest icon file sent to the interface; bigger ones are left out.
const MAX_ICON_BYTES: u64 = 512 * 1024;

/// One way to open a workspace, as the "Open in" list shows it.
#[derive(Debug, JsObject)]
pub struct OpenTarget {
    /// The editor command, or `"file-manager"`.
    pub command: String,
    /// The application's own name (`Zed`, `Dolphin`); empty when it has no desktop entry.
    pub name: String,
    /// Its icon as a `data:` URL, when one is installed.
    pub icon: Option<String>,
}

impl OpenTarget {
    /// A choice named and drawn from its desktop entry, when it has one.
    fn new(command: &str, look: Option<AppLook>) -> Self {
        let (name, icon) =
            look.map_or((String::new(), None), |look| (look.name, look.icon));
        Self {
            command: command.to_owned(),
            name,
            icon: icon.as_deref().and_then(icon_data_url),
        }
    }
}

/// An icon file as a `data:` URL the interface's `Image` loads; `None` for unknown types or oversized files.
fn icon_data_url(path: &Path) -> Option<String> {
    let mime = match path.extension()?.to_str()? {
        "svg" => "image/svg+xml",
        "png" => "image/png",
        _ => return None,
    };
    if path.metadata().ok()?.len() > MAX_ICON_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    Some(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// The desktop file id of the default application for folders (`xdg-mime`), on Linux.
fn folder_handler() -> Option<String> {
    let output = Command::new("xdg-mime")
        .args(["query", "default", "inode/directory"])
        .output()
        .ok()?;
    let id = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!id.is_empty()).then_some(id)
}

/// Editor commands looked for on `PATH`, in the order they are offered.
const EDITORS: [&str; 5] = ["zed", "code", "cursor", "codium", "subl"];

/// Whether `path` is a file this process may execute.
fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

/// The command that shows a folder in the system file manager.
fn file_manager() -> &'static str {
    if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    }
}

/// The external editors found on one `PATH`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct EditorCatalog {
    /// Command names of the installed editors, in [`EDITORS`] order.
    editors: Vec<String>,
}

impl EditorCatalog {
    /// The editors whose command is an executable file in one of `path_var`'s directories.
    fn detect(path_var: Option<&OsStr>) -> Self {
        let dirs: Vec<PathBuf> = path_var
            .map(|path| std::env::split_paths(path).collect())
            .unwrap_or_default();
        let editors = EDITORS
            .iter()
            .filter(|name| {
                dirs.iter().any(|dir| is_executable(&dir.join(name)))
            })
            .map(|name| (*name).to_owned())
            .collect();
        Self { editors }
    }

    /// `editor` when it is installed here; `None` means the file manager.
    fn resolve<'a>(&self, editor: &'a str) -> Option<&'a str> {
        self.editors
            .iter()
            .any(|known| known == editor)
            .then_some(editor)
    }
}

/// The desktop bridge for "Open in" on the local host.
#[module]
pub struct ExternalEditors;

#[module]
impl ExternalEditors {
    /// The "Open in" choices: the file manager first, then the editors installed on this process's
    /// `PATH`, each with the name and icon of its desktop entry.
    pub fn targets() -> Vec<OpenTarget> {
        let entries = DesktopEntries::from_env();
        let file_manager = folder_handler().and_then(|id| entries.by_id(&id));
        std::iter::once(OpenTarget::new(FILE_MANAGER_CHOICE, file_manager))
            .chain(
                EditorCatalog::detect(std::env::var_os("PATH").as_deref())
                    .editors
                    .iter()
                    .map(|editor| {
                        OpenTarget::new(editor, entries.by_command(editor))
                    }),
            )
            .collect()
    }

    /// Opens the local folder `path` in `editor` when it is installed, and in
    /// the file manager otherwise (including `"file-manager"`); rejects with
    /// the launch error.
    pub fn open(path: String, editor: String) -> Result<(), String> {
        let catalog =
            EditorCatalog::detect(std::env::var_os("PATH").as_deref());
        // Only a detected editor name ever reaches `Command`: the saved choice may be stale or edited.
        let program = catalog.resolve(&editor).unwrap_or(file_manager());
        // The path is one argument, not shell text, so no part of it is interpreted.
        match Command::new(program).arg(&path).spawn() {
            Ok(mut child) => {
                // Reap the launcher when it exits; the editor or file manager lives on.
                std::thread::spawn(move || child.wait());
                Ok(())
            }
            Err(error) => {
                tracing::warn!(
                    "failed to open {} in {}: {}",
                    path,
                    program,
                    error
                );
                Err(format!("{program}: {error}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn detect_lists_executables_in_editor_order() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        for (name, mode) in [("subl", 0o755), ("zed", 0o755), ("code", 0o644)] {
            let path = dir.path().join(name);
            std::fs::write(&path, "").unwrap();
            std::fs::set_permissions(
                &path,
                std::fs::Permissions::from_mode(mode),
            )
            .unwrap();
        }
        let catalog = EditorCatalog::detect(Some(dir.path().as_os_str()));
        assert_eq!(catalog.editors, ["zed", "subl"]);
    }

    #[test]
    fn detect_is_empty_without_path() {
        assert!(EditorCatalog::detect(None).editors.is_empty());
    }

    #[test]
    fn resolve_never_picks_an_unknown_editor() {
        let catalog = EditorCatalog {
            editors: vec!["zed".to_owned()],
        };
        assert_eq!(catalog.resolve("rm"), None);
        assert_eq!(catalog.resolve("file-manager"), None);
        assert_eq!(catalog.resolve("zed"), Some("zed"));
    }
}
