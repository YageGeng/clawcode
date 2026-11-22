//! Workspace commands and their key bindings.

use gpui_kit::{App, KeyBinding};

/// Key context of the workspace shell; every shell binding is scoped to it.
pub const WORKSPACE_CONTEXT: &str = "Workspace";
/// Key context around a session's message input.
pub const COMPOSER_CONTEXT: &str = "Composer";
/// Key context around the sidebar search field.
pub const SEARCH_CONTEXT: &str = "SidebarSearch";

gpui_kit::actions!(
    desktop,
    [
        /// Opens a draft session in the current workspace.
        NewSession,
        /// Closes the active tab.
        CloseTab,
        /// Activates the next tab, wrapping around.
        NextTab,
        /// Shows or hides the sidebar.
        ToggleSidebar,
        /// Shows or hides the active tab's right panel.
        TogglePanel,
        /// Closes the right panel sheet.
        ClosePanel,
        /// Shows the current workspace's files in the sidebar.
        ShowFiles,
        /// Shows the current workspace's Git changes in the sidebar.
        ShowChanges,
        /// Asks for a folder to add as a workspace.
        AddWorkspace,
        /// Opens and focuses the sidebar search field.
        FocusSearch,
        /// Closes the sidebar search field and clears its query.
        CloseSearch,
        /// Focuses the active tab's message input.
        FocusComposer,
        /// Stops the active run of the session whose composer has focus.
        StopRun,
        /// Highlights the previous slash command.
        PaletteUp,
        /// Highlights the next slash command.
        PaletteDown,
        /// Writes the highlighted slash command into the input.
        PaletteAccept,
        /// Closes the slash command list until the text changes.
        PaletteDismiss,
        /// Opens the settings tab, or activates it when already open.
        OpenSettings,
        /// Switches the active session tab between its chat and its trajectory.
        ToggleTrajectory,
        /// Quits the application.
        Quit
    ]
);

/// Activates the tab at a zero-based position.
#[derive(Clone, PartialEq, Debug, gpui_kit::Action)]
#[action(namespace = desktop, no_json)]
pub struct SelectTab {
    /// Zero-based tab position.
    pub index: usize,
}

/// Spells a shortcut with the platform's primary modifier: Cmd on macOS, Ctrl elsewhere.
macro_rules! primary {
    ($key:literal) => {
        if cfg!(target_os = "macos") {
            concat!("cmd-", $key)
        } else {
            concat!("ctrl-", $key)
        }
    };
}

/// Shortcuts for the first nine tabs, in tab order.
const TAB_KEYS: [&str; 9] = [
    primary!("1"),
    primary!("2"),
    primary!("3"),
    primary!("4"),
    primary!("5"),
    primary!("6"),
    primary!("7"),
    primary!("8"),
    primary!("9"),
];

/// Registers the shell's shortcuts.
pub fn bind_keys(cx: &mut App) {
    let context = Some(WORKSPACE_CONTEXT);
    cx.bind_keys([
        KeyBinding::new(primary!("n"), NewSession, context),
        KeyBinding::new(primary!("w"), CloseTab, context),
        KeyBinding::new("ctrl-tab", NextTab, context),
        KeyBinding::new(primary!("b"), ToggleSidebar, context),
        KeyBinding::new(primary!("j"), TogglePanel, context),
        KeyBinding::new("escape", ClosePanel, Some("PanelSheet")),
        KeyBinding::new(primary!("shift-e"), ShowFiles, context),
        KeyBinding::new(primary!("shift-g"), ShowChanges, context),
        KeyBinding::new(primary!("o"), AddWorkspace, context),
        KeyBinding::new(primary!("k"), FocusSearch, context),
        KeyBinding::new(primary!("l"), FocusComposer, context),
        // The input handles Escape first and passes it on only when it has nothing to dismiss.
        KeyBinding::new("escape", StopRun, Some(COMPOSER_CONTEXT)),
        KeyBinding::new("escape", CloseSearch, Some(SEARCH_CONTEXT)),
        KeyBinding::new(primary!(","), OpenSettings, context),
        KeyBinding::new(primary!("shift-t"), ToggleTrajectory, context),
        KeyBinding::new(primary!("q"), Quit, context),
    ]);
    // Bound at the input's own depth and registered after gpui-kit's input
    // keys, so the open list takes these keys before the input does.
    // Escape closes the list even when it shows nothing to pick; the picking
    // keys only apply with matches, so Enter still sends text like `/home/me`.
    let palette = Some("CommandPalette > Input");
    let matches = Some("CommandMatches > Input");
    cx.bind_keys([
        KeyBinding::new("up", PaletteUp, matches),
        KeyBinding::new("down", PaletteDown, matches),
        KeyBinding::new("tab", PaletteAccept, matches),
        KeyBinding::new("enter", PaletteAccept, matches),
        KeyBinding::new("escape", PaletteDismiss, palette),
    ]);
    cx.bind_keys(TAB_KEYS.iter().enumerate().map(|(index, keys)| {
        KeyBinding::new(keys, SelectTab { index }, context)
    }));
}
