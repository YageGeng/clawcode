//! Whether a question can still be settled: asked on the active branch, and
//! settled nowhere — not in the transcript, the queue, or the active Run.

use std::collections::{BTreeMap, BTreeSet};

use protocol::{
    AgentMessage, MessageContent, QUESTION_REPLY_TYPE, QuestionKey,
    QuestionReply, QuestionReplyItem, ToolResultDetails,
};
use store::EntryKind;

use crate::runtime::{KernelError, Session};

/// Questions asked on a Session's active branch and every settlement of them.
///
/// Settlements hold an admission ticket, so a ledger read under one stays
/// true until that settlement is written.
pub(in crate::runtime) struct QuestionLedger {
    asked: BTreeMap<QuestionKey, String>,
    settled: BTreeSet<QuestionKey>,
}

impl TryFrom<&Session> for QuestionLedger {
    type Error = KernelError;

    /// Scans the queue-side settlements, then the active branch.
    fn try_from(session: &Session) -> Result<Self, Self::Error> {
        // Queue-side state is read first: the Run records a reply there before
        // writing it, so a reply moving between the two is seen on one side.
        let mut settled = session.execution.settled_question_keys()?;
        let mut asked = BTreeMap::new();
        // Store entries, not `history()`: compaction drops asking results from
        // the model context while they stay on the branch. The walk borrows
        // entries; only question results and replies are copied and decoded.
        // ponytail: still one O(branch) walk per settlement; keep an index of
        // open questions if very long branches make admission slow.
        let mut questions = Vec::new();
        session.transcript.inspect_active_branch(|entry| {
            if entry.kind != EntryKind::Message {
                return;
            }
            let content = entry.payload.get("content");
            let pointer = |path: &str| {
                content
                    .and_then(|content| content.pointer(path))
                    .and_then(serde_json::Value::as_str)
            };
            if pointer("/details/type") == Some("user_questions")
                || pointer("/extension/custom_type")
                    == Some(QUESTION_REPLY_TYPE)
            {
                questions.push(entry.payload.clone());
            }
        })?;
        for payload in questions {
            let Ok(message) = serde_json::from_value::<AgentMessage>(
                serde_json::Value::Object(payload),
            ) else {
                continue;
            };
            if let MessageContent::ToolResult {
                details:
                    Some(ToolResultDetails::UserQuestions {
                        request_id,
                        questions,
                    }),
                ..
            } = message.content
            {
                asked.extend(questions.into_iter().enumerate().map(
                    |(index, question)| {
                        let key = QuestionKey {
                            request_id: request_id.clone(),
                            index,
                        };
                        (key, question.title)
                    },
                ));
            } else if let Ok(reply) = QuestionReply::try_from(&message.content)
            {
                settled.extend(reply.keys());
            }
        }
        Ok(Self { asked, settled })
    }
}

impl QuestionLedger {
    /// Reports whether `key` was asked on this branch and is settled nowhere.
    pub(in crate::runtime) fn is_open(&self, key: &QuestionKey) -> bool {
        self.asked.contains_key(key) && !self.settled.contains(key)
    }

    /// Lists every question asked on this branch that is still open.
    pub(in crate::runtime) fn open_keys(
        &self,
    ) -> impl Iterator<Item = QuestionKey> + '_ {
        self.asked.keys().filter(|key| self.is_open(key)).cloned()
    }

    /// Accepts a reply only when every question it settles is open, and
    /// copies each question's title into it for the persisted record.
    pub(in crate::runtime) fn settle(
        &self,
        reply: &mut QuestionReply,
    ) -> Result<(), KernelError> {
        for item in &mut reply.items {
            let key = QuestionKey::from(&*item);
            match self.asked.get(&key) {
                Some(title) if !self.settled.contains(&key) => {
                    item.question.clone_from(title);
                }
                _ => {
                    return Err(KernelError::QuestionClosed {
                        request_id: key.request_id,
                        index: key.index,
                    });
                }
            }
        }
        Ok(())
    }

    /// Builds the skip record for the open questions among `keys`, each
    /// once; `None` when none of them is open.
    pub(in crate::runtime) fn skip(
        &self,
        keys: impl IntoIterator<Item = QuestionKey>,
    ) -> Option<QuestionReply> {
        let mut items: Vec<QuestionReplyItem> = Vec::new();
        for key in keys {
            let title = match self.asked.get(&key) {
                Some(title) if !self.settled.contains(&key) => title,
                _ => continue,
            };
            if items.iter().any(|item| QuestionKey::from(item) == key) {
                continue;
            }
            items.push(
                QuestionReplyItem::builder()
                    .request_id(key.request_id)
                    .index(key.index)
                    .question(title.clone())
                    .build(),
            );
        }
        (!items.is_empty()).then_some(QuestionReply { items })
    }
}
