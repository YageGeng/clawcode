//! Rows of the trajectory ledger, the text of a record, and the in-flight
//! rows derived from the live projection.

use std::borrow::Cow;

use protocol::{RunId, SettingChange, TrajectoryRecord, TrajectoryRecordKind};

use crate::projection::{RunState, SessionProjection};

/// Characters a one-line summary keeps.
const SUMMARY_CHARS: usize = 200;

/// The group a record belongs to in the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RunKey {
    /// One Run.
    Run(RunId),
    /// Records between Runs, keyed by the first record's sequence.
    Between(u64),
}

/// One row of the trajectory ledger.
#[derive(Debug, Clone, PartialEq)]
pub enum LedgerRow {
    /// Loads the page before the loaded records.
    LoadEarlier,
    /// The title of one Run, or of the records between Runs.
    RunHeader {
        /// Which group.
        key: RunKey,
        /// The Run's number on the branch; `None` between Runs.
        index: Option<u32>,
        /// First start to last end of the group's records.
        duration_ms: Option<u64>,
        /// Input plus output tokens of the group's requests.
        tokens: u64,
        /// Only the title shows.
        collapsed: bool,
    },
    /// One record, by id; a request record renders as the request separator.
    Record {
        /// Record id.
        id: String,
    },
    /// The folded tool calls of one request.
    CallGroup {
        /// Number of the request that issued them.
        request_index: u32,
        /// Calls folded.
        count: usize,
        /// Sum of the finished calls' durations.
        duration_ms: u64,
        /// Calls that failed.
        failures: usize,
    },
    /// Work the history database does not hold yet.
    InFlight(InFlight),
}

/// A request or tool still running, from the live projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InFlight {
    /// A model request waiting for or streaming its answer.
    Request {
        /// When it started, in Unix milliseconds.
        started_at_ms: u64,
    },
    /// A tool executing.
    Tool {
        /// The call, which names its record once the history holds it.
        call_id: String,
        /// Tool name.
        name: String,
        /// When it started, in Unix milliseconds.
        started_at_ms: u64,
    },
}

impl InFlight {
    /// The running work of `projection`: each executing tool, or else the
    /// request after `last_ended_ms`, the end of the newest loaded record.
    pub fn from_projection(
        projection: &SessionProjection,
        last_ended_ms: Option<u64>,
    ) -> Vec<Self> {
        if projection.run_state() != RunState::Running {
            return Vec::new();
        }
        let tools: Vec<Self> = projection
            .running_tools()
            .map(|tool| Self::Tool {
                call_id: tool.tool_call_id.to_string(),
                name: tool.name.clone().unwrap_or_else(|| tool.title.clone()),
                started_at_ms: tool
                    .timestamp_ms
                    .unwrap_or_else(crate::utils::now_ms),
            })
            .collect();
        if !tools.is_empty() {
            return tools;
        }
        let started_at_ms = last_ended_ms
            .max(projection.run_started_ms())
            .unwrap_or_else(crate::utils::now_ms);
        vec![Self::Request { started_at_ms }]
    }

    /// The id of the record this work becomes: a tool call's record, which
    /// the history holds from the request's answer on; a request has none
    /// until it ends.
    pub fn record_id(&self) -> Option<String> {
        match self {
            Self::Tool { call_id, .. } => Some(format!("tool:{call_id}")),
            Self::Request { .. } => None,
        }
    }

    /// When the work started.
    pub fn started_at_ms(&self) -> u64 {
        match self {
            Self::Request { started_at_ms }
            | Self::Tool { started_at_ms, .. } => *started_at_ms,
        }
    }
}

/// The text of a record, for the ledger and its detail.
pub trait RecordText {
    /// One line of at most 200 characters.
    fn summary(&self) -> String;
    /// The record's text, arguments, and result, newline separated.
    fn text(&self) -> String;
}

impl RecordText for TrajectoryRecord {
    /// Collapses whitespace into single spaces and stops reading once the
    /// line is full, so a long tool output costs no more than a short one.
    fn summary(&self) -> String {
        let mut line = String::new();
        let mut chars = 0;
        let parts = match &self.kind {
            TrajectoryRecordKind::Tool(tool) => {
                let mut parts = vec![
                    Cow::Borrowed(tool.name.as_str()),
                    Cow::Owned(tool.arguments.to_string()),
                ];
                if let Some(result) = &tool.result_text {
                    parts.extend([
                        Cow::Borrowed("→"),
                        Cow::Borrowed(result.as_str()),
                    ]);
                }
                parts
            }
            _ => self.parts(),
        };
        for word in parts.iter().flat_map(|part| part.split_whitespace()) {
            let space = usize::from(chars > 0);
            let width = word.chars().count() + space;
            if chars + width > SUMMARY_CHARS {
                // Fill what is left, keeping one character for the ellipsis;
                // a line already full gives up its last character for it.
                let room = SUMMARY_CHARS.saturating_sub(chars + space + 1);
                if space > 0 && room > 0 {
                    line.push(' ');
                }
                line.extend(word.chars().take(room));
                if chars >= SUMMARY_CHARS {
                    line.pop();
                }
                line.push('…');
                return line;
            }
            if space > 0 {
                line.push(' ');
            }
            line.push_str(word);
            chars += width;
        }
        line
    }

    /// Joins what the record shows in its detail.
    fn text(&self) -> String {
        self.parts().join("\n")
    }
}

/// The pieces of a record's text, borrowed where the record holds them.
trait TextParts {
    /// The non-empty pieces, in reading order.
    fn parts(&self) -> Vec<Cow<'_, str>>;
}

impl TextParts for TrajectoryRecord {
    /// What the record shows in its detail, piece by piece.
    fn parts(&self) -> Vec<Cow<'_, str>> {
        fn borrowed(text: &str) -> Cow<'_, str> {
            Cow::Borrowed(text)
        }
        let parts: Vec<Cow<'_, str>> = match &self.kind {
            TrajectoryRecordKind::User(user) => vec![borrowed(&user.text)],
            TrajectoryRecordKind::Command(command) => {
                vec![borrowed(&command.text)]
            }
            TrajectoryRecordKind::Request(request) => {
                vec![borrowed(&request.snapshot.options.model_id)]
            }
            TrajectoryRecordKind::Assistant(assistant) => [
                Some(assistant.reasoning.as_str()),
                Some(assistant.text.as_str()),
                assistant.error.as_deref(),
            ]
            .into_iter()
            .flatten()
            .map(borrowed)
            .collect(),
            TrajectoryRecordKind::Tool(tool) => vec![
                borrowed(&tool.name),
                Cow::Owned(tool.arguments.to_string()),
                borrowed(tool.result_text.as_deref().unwrap_or_default()),
            ],
            TrajectoryRecordKind::Compaction { summary, .. }
            | TrajectoryRecordKind::BranchSummary { summary } => {
                vec![borrowed(summary)]
            }
            TrajectoryRecordKind::Setting(change) => vec![match change {
                SettingChange::Model { provider, model } => {
                    Cow::Owned(format!("{provider}/{model}"))
                }
                SettingChange::ThinkingLevel { level } => Cow::Owned(
                    serde_json::to_value(level)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .unwrap_or_default(),
                ),
                SettingChange::ActiveTools { tools } => {
                    Cow::Owned(tools.join(", "))
                }
            }],
            TrajectoryRecordKind::Extension(extension) => {
                vec![borrowed(&extension.text)]
            }
        };
        parts.into_iter().filter(|part| !part.is_empty()).collect()
    }
}
