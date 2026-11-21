//! UI-independent transcript layout: tool groups, folded runs, and minimal splices.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use agent_client_protocol::v2::schema as wire;

use crate::projection::{
    MessageOrigin, ModelUsage, Role, RowKey, RunState, SessionProjection,
    ToolCategory, TranscriptRow,
};

/// Where a tool row sits inside its group of consecutive tool rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupPosition {
    /// The only tool in its group.
    Only,
    /// First of several.
    First,
    /// Neither first nor last.
    Middle,
    /// Last of several.
    Last,
}

/// What a folded run did, shown in place of its tool rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunSummary {
    /// Time from the prompt to the last row's end, when both are known.
    pub elapsed_ms: Option<u64>,
    /// Non-zero tool counts in `ToolCategory` declaration order.
    pub counts: Vec<(ToolCategory, usize)>,
}

/// What a finished run used: summed tokens, the model, and its duration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunUsage {
    /// `(provider_id, model_id)` of the run's last reporting reply.
    pub model: Option<(String, String)>,
    /// Token counts summed over the run's replies.
    pub usage: ModelUsage,
    /// Time from the prompt to the last row's end, when known.
    pub elapsed_ms: Option<u64>,
}

/// One displayed row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutRow {
    /// A projection row.
    Row {
        /// Identity of the row.
        key: RowKey,
        /// Index into the projection.
        index: usize,
        /// Group position for tool rows outside a folded run.
        group: Option<GroupPosition>,
    },
    /// A finished run's summary: in place of its tools when folded, above them when unfolded.
    RunSummary {
        /// Key of the prompt that starts the run.
        anchor: RowKey,
        /// What the run did.
        summary: RunSummary,
        /// Whether the run's tools are shown below the summary.
        expanded: bool,
    },
    /// A finished run's token usage, below its last reply.
    Usage {
        /// Key of the reply it follows.
        anchor: RowKey,
        /// What the run used.
        usage: RunUsage,
    },
}

impl LayoutRow {
    /// Equality for diffing: a row's projection index shifts when rows are
    /// inserted before it, which must not break the unchanged suffix.
    fn same_display(&self, other: &LayoutRow) -> bool {
        match (self, other) {
            (
                LayoutRow::Row { key, group, .. },
                LayoutRow::Row {
                    key: k2, group: g2, ..
                },
            ) => key == k2 && group == g2,
            _ => self == other,
        }
    }

    /// Stable element id: the row key, `run:{anchor}` for a summary, or `usage:{anchor}`.
    pub fn element_id(&self) -> String {
        match self {
            LayoutRow::Row { key, .. } => key.to_string(),
            LayoutRow::RunSummary { anchor, .. } => format!("run:{anchor}"),
            LayoutRow::Usage { anchor, .. } => format!("usage:{anchor}"),
        }
    }
}

/// The splice that turns one layout into the next, plus rows to re-measure.
#[derive(Debug, PartialEq, Eq)]
pub struct LayoutDiff {
    /// `(removed old range, inserted count)`; `None` when the rows are equal.
    pub splice: Option<(Range<usize>, usize)>,
    /// Indexes in the new layout of unchanged-position rows whose content changed.
    pub remeasure: Vec<usize>,
}

/// Displayed rows for a projection.
#[derive(Default)]
pub struct TranscriptLayout {
    rows: Vec<LayoutRow>,
}

/// Folds one projection into layout rows, one run segment at a time.
struct LayoutBuilder<'a> {
    projection: &'a SessionProjection,
    expanded_runs: &'a HashSet<RowKey>,
    rows: Vec<LayoutRow>,
}

impl<'a> LayoutBuilder<'a> {
    /// Splits visible rows into segments that each start at a user prompt.
    fn segments(&self) -> Vec<Vec<(usize, &'a TranscriptRow)>> {
        let mut segments: Vec<Vec<(usize, &TranscriptRow)>> = Vec::new();
        for index in 0..self.projection.len() {
            let Some(row) = self.projection.row(index) else {
                continue;
            };
            let starts_run = match row {
                TranscriptRow::Message(message) => {
                    // A user-run command is drawn by its execute tool row, which
                    // shares the message id, so the `!cmd` message is skipped.
                    let shown_by_tool = message.origin
                        == MessageOrigin::UserBash
                        && self
                            .projection
                            // Shares the message id's `Arc` instead of copying the text.
                            .index_of(&RowKey::Tool(wire::ToolCallId::from(
                                Arc::clone(&message.message_id.0),
                            )))
                            .is_some();
                    if !message.is_visible() || shown_by_tool {
                        continue;
                    }
                    message.role == Role::User
                        && message.origin == MessageOrigin::Prompt
                }
                // A task sent to a spawned agent starts its run like a prompt.
                TranscriptRow::Mail(mail) => mail.kind == "new_task",
                TranscriptRow::Tool(_)
                | TranscriptRow::Compaction(_)
                | TranscriptRow::Error(_) => false,
            };
            if starts_run || segments.is_empty() {
                segments.push(Vec::new());
            }
            if let Some(segment) = segments.last_mut() {
                segment.push((index, row));
            }
        }
        segments
    }

    /// Timestamp of the segment's first row, its anchor.
    fn anchor_ms(segment: &[(usize, &TranscriptRow)]) -> Option<u64> {
        match segment.first()?.1 {
            TranscriptRow::Message(message) => message.timestamp_ms,
            TranscriptRow::Tool(tool) => tool.timestamp_ms,
            TranscriptRow::Compaction(_)
            | TranscriptRow::Error(_)
            | TranscriptRow::Mail(_) => None,
        }
    }

    /// Elapsed time from the anchor's timestamp to the latest row end.
    fn elapsed_ms(segment: &[(usize, &TranscriptRow)]) -> Option<u64> {
        let start = Self::anchor_ms(segment)?;
        let end = segment
            .iter()
            .filter_map(|(_, row)| match row {
                TranscriptRow::Message(message) => message
                    .timestamp_ms
                    .map(|ts| ts + message.duration_ms.unwrap_or(0)),
                TranscriptRow::Tool(tool) => tool.timestamp_ms,
                TranscriptRow::Compaction(_)
                | TranscriptRow::Error(_)
                | TranscriptRow::Mail(_) => None,
            })
            .max()?;
        Some(end.saturating_sub(start))
    }

    /// Appends one segment, folding its tools when the run is finished.
    fn push_segment(
        &mut self,
        segment: &[(usize, &TranscriptRow)],
        finished: bool,
    ) {
        let Some(anchor) = segment.first().map(|(_, row)| row.key()) else {
            return;
        };
        let has_tools = segment
            .iter()
            .any(|(_, row)| matches!(row, TranscriptRow::Tool(_)));
        let summarize = finished && has_tools;
        let expanded = self.expanded_runs.contains(&anchor);
        let fold = summarize && !expanded;
        let mut summary_placed = false;
        // Position in `self.rows` of the segment's last reply, where usage goes.
        let mut last_reply: Option<(usize, RowKey)> = None;
        // Index in `self.rows` where the open run of consecutive tool rows began.
        let mut group_start: Option<usize> = None;
        for (index, row) in segment {
            let is_tool = matches!(row, TranscriptRow::Tool(_));
            // The summary goes where the first tool was; an unfolded run keeps it
            // as the header of its tools so it can be folded again.
            if summarize && is_tool {
                if !summary_placed {
                    summary_placed = true;
                    self.close_group(&mut group_start);
                    // One pass over the segment; `ALL` is in declaration
                    // order, so a category's discriminant is its slot.
                    let mut totals = [0usize; ToolCategory::ALL.len()];
                    for (_, row) in segment {
                        if let TranscriptRow::Tool(tool) = row
                            && let Some(total) =
                                totals.get_mut(tool.category() as usize)
                        {
                            *total += 1;
                        }
                    }
                    let counts = ToolCategory::ALL
                        .into_iter()
                        .zip(totals)
                        .filter(|(_, total)| *total > 0)
                        .collect();
                    self.rows.push(LayoutRow::RunSummary {
                        anchor: anchor.clone(),
                        summary: RunSummary {
                            elapsed_ms: Self::elapsed_ms(segment),
                            counts,
                        },
                        expanded,
                    });
                }
                if fold {
                    continue;
                }
            }
            if is_tool {
                group_start.get_or_insert(self.rows.len());
            } else {
                self.close_group(&mut group_start);
            }
            if let TranscriptRow::Message(message) = row
                && message.role == Role::Assistant
            {
                last_reply = Some((self.rows.len(), row.key()));
            }
            self.rows.push(LayoutRow::Row {
                key: row.key(),
                index: *index,
                group: None,
            });
        }
        self.close_group(&mut group_start);
        if finished
            && let Some((position, anchor)) = last_reply
            && let Some(usage) = Self::run_usage(segment)
        {
            self.rows
                .insert(position + 1, LayoutRow::Usage { anchor, usage });
        }
    }

    /// Sums the usage the segment's replies report; `None` when none reports any.
    fn run_usage(segment: &[(usize, &TranscriptRow)]) -> Option<RunUsage> {
        let mut total: Option<ModelUsage> = None;
        let mut model = None;
        for (_, row) in segment {
            let TranscriptRow::Message(message) = row else {
                continue;
            };
            let Some(assistant) = &message.assistant else {
                continue;
            };
            model = Some((
                assistant.provider_id.clone(),
                assistant.model_id.clone(),
            ));
            if let Some(usage) = &assistant.usage {
                total = Some(match total {
                    Some(sum) => sum.sum(usage),
                    None => usage.clone(),
                });
            }
        }
        Some(RunUsage {
            model,
            usage: total?,
            elapsed_ms: Self::elapsed_ms(segment),
        })
    }

    /// Labels the open tool group (rows from its start to the end) once, then closes it.
    fn close_group(&mut self, start: &mut Option<usize>) {
        let Some(start) = start.take() else { return };
        let len = self.rows.len().saturating_sub(start);
        for (offset, row) in self.rows.iter_mut().skip(start).enumerate() {
            if let LayoutRow::Row { group, .. } = row {
                *group = Some(match (len, offset) {
                    (1, _) => GroupPosition::Only,
                    (_, 0) => GroupPosition::First,
                    (_, last) if last + 1 == len => GroupPosition::Last,
                    _ => GroupPosition::Middle,
                });
            }
        }
    }
}

impl TranscriptLayout {
    /// Builds the layout: skips empty messages, folds finished runs, groups tools.
    pub fn build(
        projection: &SessionProjection,
        expanded_runs: &HashSet<RowKey>,
    ) -> Self {
        let mut builder = LayoutBuilder {
            projection,
            expanded_runs,
            rows: Vec::new(),
        };
        let segments = builder.segments();
        let idle = projection.run_state() == RunState::Idle;
        // Steering and mid-run follow-ups start new segments inside the same
        // run, so every segment anchored since the run began is still live.
        let run_started = projection.run_started_ms().filter(|_| !idle);
        for (position, segment) in segments.iter().enumerate() {
            let last_live = position + 1 == segments.len() && !idle;
            let in_run = run_started.is_some_and(|started| {
                LayoutBuilder::anchor_ms(segment)
                    .is_some_and(|anchor| anchor >= started)
            });
            builder.push_segment(segment, !(last_live || in_run));
        }
        Self { rows: builder.rows }
    }

    /// Number of displayed rows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether nothing is displayed.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The displayed row at `ix`.
    pub fn row(&self, ix: usize) -> Option<&LayoutRow> {
        self.rows.get(ix)
    }

    /// Position of the projection row with this key, if displayed.
    pub fn position_of(&self, key: &RowKey) -> Option<usize> {
        self.rows.iter().position(
            |row| matches!(row, LayoutRow::Row { key: found, .. } if found == key),
        )
    }

    /// The diff of a batch that only grew the content of `updated` rows:
    /// nothing moves, and the displayed ones need re-measuring.
    pub fn content_diff(&self, updated: &HashSet<RowKey>) -> LayoutDiff {
        // ponytail: O(n) scan per batch; keep a key-to-position map if long
        // transcripts ever make streaming stutter.
        let remeasure = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                matches!(row, LayoutRow::Row { key, .. } if updated.contains(key))
            })
            .map(|(ix, _)| ix)
            .collect();
        LayoutDiff {
            splice: None,
            remeasure,
        }
    }

    /// Minimal splice from `self` to `next`; rows kept at the edges whose key is in `updated` need re-measuring.
    pub fn diff(
        &self,
        next: &TranscriptLayout,
        updated: &HashSet<RowKey>,
    ) -> LayoutDiff {
        let (old, new) = (&self.rows, &next.rows);
        let prefix = old
            .iter()
            .zip(new)
            .take_while(|(a, b)| a.same_display(b))
            .count();
        let limit = old.len().min(new.len()) - prefix;
        let suffix = old
            .iter()
            .rev()
            .zip(new.iter().rev())
            .take(limit)
            .take_while(|(a, b)| a.same_display(b))
            .count();
        let splice = (prefix != old.len() || prefix != new.len())
            .then(|| (prefix..old.len() - suffix, new.len() - prefix - suffix));
        let remeasure = new
            .iter()
            .enumerate()
            .filter(|(ix, row)| {
                (*ix < prefix || *ix >= new.len() - suffix)
                    && matches!(row, LayoutRow::Row { key, .. } if updated.contains(key))
            })
            .map(|(ix, _)| ix)
            .collect();
        LayoutDiff { splice, remeasure }
    }
}
