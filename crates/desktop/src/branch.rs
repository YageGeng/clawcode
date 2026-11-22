//! The session-tree entries behind transcript messages, for the message
//! actions (M4b spec §3): Branch moves to a question's parent entry, Fork
//! copies history up to a reply's entry.

use protocol::{SessionTreeEntry, SessionTreeSnapshot};

/// Who wrote a message, as its tree entry's `payload.content.type` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageAuthor {
    /// The user's question.
    User,
    /// The model's reply.
    Assistant,
}

impl MessageAuthor {
    /// The content type the tree entry carries.
    fn content_type(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

/// Finds the tree entry behind a transcript message.
pub trait MessageEntries {
    /// The `message` entry whose `payload.message_id` is `message_id` and
    /// whose content `author` wrote.
    fn message_entry(
        &self,
        message_id: &str,
        author: MessageAuthor,
    ) -> Option<&SessionTreeEntry>;
}

impl MessageEntries for SessionTreeSnapshot {
    /// Scans the entries; a message id appears once per branch it is on, and
    /// any of those entries has the same parent.
    fn message_entry(
        &self,
        message_id: &str,
        author: MessageAuthor,
    ) -> Option<&SessionTreeEntry> {
        self.entries.iter().find(|entry| {
            entry.kind == "message"
                && entry.payload.get("message_id").and_then(|id| id.as_str())
                    == Some(message_id)
                && entry
                    .payload
                    .pointer("/content/type")
                    .and_then(|kind| kind.as_str())
                    == Some(author.content_type())
        })
    }
}
