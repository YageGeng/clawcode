//! "Open in" (M3b spec §5.4): the external editors installed on this machine,
//! and opening a workspace in one of them or in the file manager.

use std::ffi::OsStr;
use std::path::Path;

use gpui_kit::Context;

use crate::state::OpenIn;
use crate::ui::AppView;

/// Editor commands looked for on `PATH`, in the order they are offered.
const EDITORS: [&str; 5] = ["zed", "code", "cursor", "codium", "subl"];

/// The external editors found on `PATH`, detected once at startup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditorCatalog {
    /// Command names of the installed editors, in [`EDITORS`] order.
    editors: Vec<String>,
}

impl EditorCatalog {
    /// The editors whose command is an executable file in one of `path_var`'s directories.
    pub fn detect(path_var: Option<&OsStr>) -> Self {
        let dirs: Vec<_> = path_var
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

    /// Command names of the installed editors.
    pub fn editors(&self) -> &[String] {
        &self.editors
    }

    /// Whether `name` is an installed editor.
    pub fn contains(&self, name: &str) -> bool {
        self.editors.iter().any(|editor| editor == name)
    }
}

/// Whether `path` is a regular file someone may execute.
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::metadata(path).is_ok_and(|metadata| {
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    })
}

impl OpenIn {
    /// The choice that applies here: a saved editor that is no longer installed opens the file manager.
    pub fn resolve(&self, catalog: &EditorCatalog) -> OpenIn {
        match self {
            OpenIn::Editor(name) if catalog.contains(name) => self.clone(),
            OpenIn::Editor(_) | OpenIn::FileManager => OpenIn::FileManager,
        }
    }
}

impl AppView {
    /// Opens `path` the way `target` says; a failed editor launch is reported as a notice.
    pub(in crate::ui) fn open_workspace_in(
        &mut self,
        target: &OpenIn,
        path: &Path,
        cx: &mut Context<'_, Self>,
    ) {
        match target.resolve(&self.details.editors) {
            OpenIn::FileManager => self.reveal_workspace(path, cx),
            OpenIn::Editor(name) => {
                match std::process::Command::new(&name).arg(path).spawn() {
                    Ok(mut child) => {
                        // Reap the editor's launcher when it exits; the editor itself lives on.
                        std::thread::spawn(move || child.wait());
                    }
                    Err(error) => {
                        tracing::warn!(
                            "failed to open {} in {}: {}",
                            path.display(),
                            name,
                            error
                        );
                        self.show_notice(
                            rust_i18n::t!("Details.open_failed", name = name)
                                .into_owned(),
                            Some(format!("{name} {}: {error}", path.display())),
                            cx,
                        );
                    }
                }
            }
        }
    }
}
