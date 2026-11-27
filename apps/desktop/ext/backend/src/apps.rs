//! What the desktop knows of an installed application from its freedesktop
//! `.desktop` entry: the name it shows and its icon, for "Open in" choices.

use std::path::{Path, PathBuf};

/// An application's display name and icon file, from its desktop entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppLook {
    /// `Name=`.
    pub(crate) name: String,
    /// The icon file `Icon=` resolves to, if one is installed.
    pub(crate) icon: Option<PathBuf>,
}

/// The `share` directories applications and icons are installed under, most specific first.
pub(crate) struct DesktopEntries {
    dirs: Vec<PathBuf>,
}

/// Icon sizes looked for in the hicolor theme, largest first, after the scalable one.
const ICON_SIZES: [&str; 6] =
    ["256x256", "128x128", "96x96", "64x64", "48x48", "32x32"];

impl DesktopEntries {
    /// The XDG data directories of this process: `$XDG_DATA_HOME` (or `~/.local/share`), then `$XDG_DATA_DIRS`.
    pub(crate) fn from_env() -> Self {
        let home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| Path::new(&home).join(".local/share"))
            });
        let system = std::env::var_os("XDG_DATA_DIRS")
            .filter(|dirs| !dirs.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        Self {
            dirs: home
                .into_iter()
                .chain(std::env::split_paths(&system))
                .collect(),
        }
    }

    /// The entry of the application whose `Exec=` runs `command`, skipping hidden helper entries (`NoDisplay=true`).
    pub(crate) fn by_command(&self, command: &str) -> Option<AppLook> {
        self.entries().find_map(|text| {
            let entry = Entry::parse(&text)?;
            (!entry.no_display && entry.command() == Some(command))
                .then(|| self.look(entry))
        })
    }

    /// The entry with desktop file id `id` (`org.kde.dolphin.desktop`).
    pub(crate) fn by_id(&self, id: &str) -> Option<AppLook> {
        self.dirs.iter().find_map(|dir| {
            let text =
                std::fs::read_to_string(dir.join("applications").join(id))
                    .ok()?;
            Entry::parse(&text).map(|entry| self.look(entry))
        })
    }

    /// The installed file of icon `name`: an absolute path as is, else the hicolor theme, else `pixmaps`.
    pub(crate) fn icon_file(&self, name: &str) -> Option<PathBuf> {
        let path = Path::new(name);
        if path.is_absolute() {
            return path.is_file().then(|| path.to_path_buf());
        }
        self.dirs.iter().find_map(|dir| {
            let hicolor = dir.join("icons/hicolor");
            std::iter::once(hicolor.join(format!("scalable/apps/{name}.svg")))
                .chain(ICON_SIZES.iter().map(|size| {
                    hicolor.join(format!("{size}/apps/{name}.png"))
                }))
                .chain([
                    dir.join(format!("pixmaps/{name}.svg")),
                    dir.join(format!("pixmaps/{name}.png")),
                ])
                .find(|candidate| candidate.is_file())
        })
    }

    /// The `.desktop` files under every `applications` directory, as text, most specific first.
    fn entries(&self) -> impl Iterator<Item = String> + '_ {
        self.dirs
            .iter()
            .filter_map(|dir| std::fs::read_dir(dir.join("applications")).ok())
            .flat_map(|files| files.flatten())
            .filter(|file| {
                file.path().extension().is_some_and(|ext| ext == "desktop")
            })
            .filter_map(|file| std::fs::read_to_string(file.path()).ok())
    }

    /// The look of a parsed entry, with its icon resolved to a file.
    fn look(&self, entry: Entry<'_>) -> AppLook {
        AppLook {
            name: entry.name.to_owned(),
            icon: entry.icon.and_then(|icon| self.icon_file(icon)),
        }
    }
}

/// The keys of a desktop entry's `[Desktop Entry]` group that "Open in" reads.
struct Entry<'a> {
    name: &'a str,
    exec: Option<&'a str>,
    icon: Option<&'a str>,
    no_display: bool,
}

impl<'a> Entry<'a> {
    /// Reads `Name=`, `Exec=`, `Icon=` and `NoDisplay=` of the main group; `None` without a name.
    fn parse(text: &'a str) -> Option<Self> {
        let (mut name, mut exec, mut icon) = (None, None, None);
        let mut no_display = false;
        let mut main = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                // Actions (`[Desktop Action new-window]`) repeat the keys; only the main group counts.
                main = line == "[Desktop Entry]";
            } else if main {
                if let Some(value) = line.strip_prefix("Name=") {
                    name.get_or_insert(value);
                } else if let Some(value) = line.strip_prefix("Exec=") {
                    exec.get_or_insert(value);
                } else if let Some(value) = line.strip_prefix("Icon=") {
                    icon.get_or_insert(value);
                } else if let Some(value) = line.strip_prefix("NoDisplay=") {
                    no_display = value.eq_ignore_ascii_case("true");
                }
            }
        }
        Some(Self {
            name: name?,
            exec,
            icon,
            no_display,
        })
    }

    /// The program `Exec=` runs, by file name (`/usr/bin/code --new` is `code`).
    fn command(&self) -> Option<&'a str> {
        let program = self.exec?.split_whitespace().next()?;
        Path::new(program).file_name()?.to_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A data directory holding `files` (relative path, contents).
    fn share(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, contents) in files {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        dir
    }

    const ZED: &str = "[Desktop Entry]\nName=Zed\nExec=/usr/bin/zed %U\nIcon=zed\n\n[Desktop Action new]\nName=New Window\nExec=zed --new\n";

    #[test]
    fn an_editor_is_found_by_the_program_it_runs() {
        let dir = share(&[
            ("applications/dev.zed.Zed.desktop", ZED),
            ("icons/hicolor/scalable/apps/zed.svg", "<svg/>"),
        ]);
        let entries = DesktopEntries {
            dirs: vec![dir.path().to_path_buf()],
        };
        let look = entries.by_command("zed").unwrap();
        assert_eq!(look.name, "Zed");
        assert_eq!(
            look.icon,
            Some(dir.path().join("icons/hicolor/scalable/apps/zed.svg"))
        );
        assert!(entries.by_command("code").is_none());
    }

    #[test]
    fn a_file_manager_is_found_by_its_desktop_id() {
        let dir = share(&[
            (
                "applications/org.kde.dolphin.desktop",
                "[Desktop Entry]\nName=Dolphin\nExec=dolphin %u\nIcon=system-file-manager\n",
            ),
            ("icons/hicolor/48x48/apps/system-file-manager.png", "png"),
        ]);
        let entries = DesktopEntries {
            dirs: vec![dir.path().to_path_buf()],
        };
        let look = entries.by_id("org.kde.dolphin.desktop").unwrap();
        assert_eq!(look.name, "Dolphin");
        assert_eq!(
            look.icon,
            Some(
                dir.path()
                    .join("icons/hicolor/48x48/apps/system-file-manager.png")
            )
        );
    }

    #[test]
    fn icons_prefer_scalable_then_the_largest_bitmap_then_pixmaps() {
        let dir = share(&[
            ("icons/hicolor/32x32/apps/a.png", "png"),
            ("icons/hicolor/128x128/apps/a.png", "png"),
            ("pixmaps/b.png", "png"),
        ]);
        let entries = DesktopEntries {
            dirs: vec![dir.path().to_path_buf()],
        };
        assert_eq!(
            entries.icon_file("a"),
            Some(dir.path().join("icons/hicolor/128x128/apps/a.png"))
        );
        assert_eq!(
            entries.icon_file("b"),
            Some(dir.path().join("pixmaps/b.png"))
        );
        assert_eq!(entries.icon_file("missing"), None);
    }

    #[test]
    fn command_lookup_skips_no_display_helper_entries() {
        let dir = share(&[
            (
                "applications/a-code-url-handler.desktop",
                "[Desktop Entry]\nName=Visual Studio Code - URL Handler\nExec=/usr/bin/code --open-url %U\nNoDisplay=true\n",
            ),
            (
                "applications/z-code.desktop",
                "[Desktop Entry]\nName=Visual Studio Code\nExec=/usr/bin/code %F\nIcon=vscode\n",
            ),
        ]);
        let entries = DesktopEntries {
            dirs: vec![dir.path().to_path_buf()],
        };
        assert_eq!(
            entries.by_command("code").unwrap().name,
            "Visual Studio Code"
        );
    }
}
