//! Small context-free helpers with no natural owner. A helper that serves one
//! type or one view belongs there instead.

use std::path::Path;

use gpui_kit::{Decorations, Pixels, Size, Window, WindowBounds, px};

/// The current Unix time in milliseconds; `0` when the clock reads before the
/// epoch, `u64::MAX` past the year 584 million.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// A non-negative value as a whole number, saturating; float to
/// integer casts saturate in Rust, and the value is clamped at zero first.
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
pub fn whole(value: f64) -> u64 {
    value.max(0.0) as u64
}

/// One display line of text that may span several: the first non-empty line,
/// trimmed, followed by ` …` when more non-empty text comes after it.
///
/// Rows of fixed height (tool targets, user-run commands, titles) would
/// otherwise lay out the extra lines and clip them.
pub fn single_line(text: &str) -> std::borrow::Cow<'_, str> {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    match (lines.next(), lines.next()) {
        (None, _) => std::borrow::Cow::Borrowed(""),
        (Some(first), None) => std::borrow::Cow::Borrowed(first),
        (Some(first), Some(_)) => {
            std::borrow::Cow::Owned(format!("{first} \u{2026}"))
        }
    }
}

/// `text` cut to `limit` characters, with an ellipsis when cut.
pub fn shorten(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((end, _)) => format!("{}…", text.get(..end).unwrap_or(text)),
        None => text.to_owned(),
    }
}

/// `text` as a Markdown code block in `language`, fenced with more
/// backticks than any run inside it so the text cannot close the block.
pub fn fenced(text: &str, language: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for ch in text.chars() {
        run = if ch == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}{language}\n{text}\n{fence}")
}

/// Pretty JSON of `value`.
pub fn pretty(value: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// Splits a root-relative path into its directory (empty at the root) and file name.
pub fn split_path(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

/// Whether `path` is a regular file someone may execute.
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::metadata(path).is_ok_and(|metadata| {
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    })
}

/// The width the shell is laid out in: the viewport without the shadow that
/// client-side decorations draw inside it, beside each edge that is not tiled.
pub fn shell_width(window: &Window) -> f32 {
    let inset = window.client_inset().unwrap_or(px(0.));
    let shadows = match window.window_decorations() {
        Decorations::Server => 0,
        Decorations::Client { tiling } => {
            u8::from(!tiling.left) + u8::from(!tiling.right)
        }
    };
    f32::from(window.viewport_size().width - inset * f32::from(shadows))
}

/// Grows a new window whose `saved` size the platform took as its outer size.
///
/// The saved size leaves out the client-side decoration shadow. Some
/// compositors take it as the size including that shadow, which would shrink
/// the window by the shadow on every launch. A compositor that chose the size
/// itself, such as a tiling one, is left alone.
pub fn restore_inner_size(saved: Size<Pixels>, window: &Window) {
    // The shadow is known once the first frame has been drawn.
    window.on_next_frame(move |window, _| {
        window.on_next_frame(move |window, _| {
            let (WindowBounds::Windowed(outer), WindowBounds::Windowed(inner)) =
                (window.window_bounds(), window.inner_window_bounds())
            else {
                return;
            };
            if outer.size == saved && inner.size != saved {
                window.resize(saved + (outer.size - inner.size));
            }
        });
    });
}
