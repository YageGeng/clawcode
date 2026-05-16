//! Terminal setup and restore helpers.

use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::{
    event::{DisableBracketedPaste, EnableBracketedPaste},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

/// Convenience alias for the ratatui terminal type used by this crate.
pub type TuiTerminal = Terminal<CrosstermBackend<Stdout>>;

/// RAII guard that restores terminal state when dropped.
pub struct TerminalGuard {
    alt_screen: bool,
    restored: bool,
}

/// Enters terminal TUI mode and returns a terminal handle together with a restore guard.
pub fn enter(use_alt_screen: bool) -> Result<(TuiTerminal, TerminalGuard)> {
    let mut stdout = io::stdout();
    // Enable raw mode first so key and resize events are stable for TUI rendering.
    if let Err(error) = enable_raw_mode() {
        return Err(error.into());
    }

    // Bracketed paste must be enabled before entering any alternate screen flow.
    if let Err(error) = execute!(stdout, EnableBracketedPaste) {
        let _ = disable_raw_mode();
        return Err(error.into());
    }

    // Enter alternate screen only when requested and roll back immediately on failure.
    let alt_screen = if use_alt_screen {
        match execute!(stdout, EnterAlternateScreen) {
            Ok(()) => true,
            Err(error) => {
                let _ = execute!(stdout, DisableBracketedPaste);
                let _ = disable_raw_mode();
                return Err(error.into());
            }
        }
    } else {
        false
    };

    // Create the terminal at the end so any setup failure still gets fully rolled back.
    let terminal = match Terminal::new(CrosstermBackend::new(io::stdout())) {
        Ok(terminal) => terminal,
        Err(error) => {
            if alt_screen {
                let _ = execute!(stdout, LeaveAlternateScreen);
            }
            let _ = execute!(stdout, DisableBracketedPaste);
            let _ = disable_raw_mode();
            return Err(error.into());
        }
    };

    let guard = TerminalGuard {
        alt_screen,
        restored: false,
    };
    Ok((terminal, guard))
}

impl TerminalGuard {
    /// Restores terminal state and disables TUI mode helpers.
    pub fn restore(&mut self) -> Result<()> {
        if self.restored {
            return Ok(());
        }

        let mut errors = Vec::<anyhow::Error>::new();
        let mut stdout = io::stdout();
        if self.alt_screen
            && let Err(error) = execute!(stdout, LeaveAlternateScreen)
        {
            errors.push(error.into());
        }
        if let Err(error) = execute!(stdout, DisableBracketedPaste) {
            errors.push(error.into());
        }
        if let Err(error) = disable_raw_mode() {
            errors.push(error.into());
        }

        self.restored = true;

        if let Some(error) = errors.into_iter().next() {
            return Err(error);
        }

        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
