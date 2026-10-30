use agent_client_protocol::schema::v2 as wire;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use protocol::{ContentBlock, UserBashDisposition, UserBashResult};

/// One terminal's whole visible output, sent as an authoritative replacement.
///
/// shortcut: never sends `terminal_output_chunk`, because the Kernel publishes
/// bounded tail snapshots and increments cannot be derived from them; upgrade
/// once the Kernel streams output bytes.
pub(crate) struct TerminalSnapshot<'a> {
    pub(crate) terminal_id: &'a str,
    pub(crate) output: &'a str,
}

impl From<TerminalSnapshot<'_>> for wire::TerminalUpdate {
    /// Base64-encodes the bytes so ANSI escapes and any UTF-8 survive unchanged.
    fn from(snapshot: TerminalSnapshot<'_>) -> Self {
        Self::new(snapshot.terminal_id.to_string()).output(
            wire::TerminalOutput::new(BASE64_STANDARD.encode(snapshot.output)),
        )
    }
}

/// How a terminal's process ended; unknown parts stay absent rather than invented.
pub(crate) struct TerminalExit<'a> {
    pub(crate) exit_code: Option<i32>,
    pub(crate) signal: Option<&'a str>,
}

impl From<TerminalExit<'_>> for wire::TerminalExitStatus {
    /// Omits whichever of exit code and signal is unknown.
    fn from(exit: TerminalExit<'_>) -> Self {
        Self::new()
            .exit_code(exit.exit_code.map(i64::from))
            .signal(exit.signal.map(str::to_string))
    }
}

/// Borrows tool result blocks for the text a terminal displays.
pub(crate) struct TerminalText<'a>(pub(crate) &'a [ContentBlock]);

impl From<TerminalText<'_>> for String {
    /// Joins the text blocks; a bash result carries no other kind.
    fn from(text: TerminalText<'_>) -> Self {
        text.0
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// Borrows a `!bash` result for the status of the tool that displays it.
pub(crate) struct ShellStatusOf<'a>(pub(crate) &'a UserBashResult);

impl From<ShellStatusOf<'_>> for wire::ToolCallStatus {
    /// Cancellation wins; a blocked run, non-zero exit, or signal exit is a failure.
    fn from(status: ShellStatusOf<'_>) -> Self {
        let result = status.0;
        let failed =
            matches!(result.disposition, UserBashDisposition::Blocked { .. })
                || result.exit_code.is_some_and(|code| code != 0)
                || (result.exit_code.is_none() && result.signal.is_some());
        wire::ToolCallStatus::from(crate::tool_call::ToolStatusOf {
            cancelled: result.cancelled,
            is_error: failed,
        })
    }
}
