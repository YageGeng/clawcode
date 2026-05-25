//! Line-ending normalization for hashline edit application.

use std::borrow::Cow;

/// Line ending style detected in the original file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LineEnding {
    Lf,
    Crlf,
}

impl LineEnding {
    /// Detect the first line ending style used by the file content.
    pub(super) fn detect(content: &str) -> Self {
        match (content.find("\r\n"), content.find('\n')) {
            (Some(crlf), Some(lf)) if crlf == lf.saturating_sub(1) => Self::Crlf,
            (Some(_), None) => Self::Crlf,
            _ => Self::Lf,
        }
    }

    /// Restore this line ending style to LF-normalized owned content.
    pub(super) fn restore_owned(self, content: String) -> String {
        match self {
            Self::Lf => content,
            Self::Crlf => content.replace('\n', "\r\n"),
        }
    }
}

/// Normalize all common line endings to LF before hashline edit application.
pub(super) fn normalize_to_lf(content: &str) -> Cow<'_, str> {
    if content.contains('\r') {
        Cow::Owned(content.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(content)
    }
}
