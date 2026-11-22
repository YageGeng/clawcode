//! The loaded trajectory of one Session and what the ledger shows of it:
//! page merging, Run groups, folding, time interval, and selection.
//! Pure data, so it is tested without a window.

mod rows;
pub mod timeline;

use std::collections::{BTreeMap, HashMap, HashSet};

use protocol::{
    EntryId, ModelUsage, RequestRecord, TrajectoryCursor, TrajectoryPage,
    TrajectoryRecord, TrajectoryRecordKind,
};
use typed_builder::TypedBuilder;

pub use rows::{InFlight, LedgerRow, RecordText, RunKey};

/// A closed time range in Unix milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeInterval {
    /// Start, inclusive.
    pub start_ms: u64,
    /// End, inclusive.
    pub end_ms: u64,
}

impl TimeInterval {
    /// Whether the record's span meets this interval; a record with only a
    /// start is a point, one without a start never meets it.
    pub fn overlaps(&self, record: &TrajectoryRecord) -> bool {
        let Some(start) = record.started_at_ms.map(|at| at.get()) else {
            return false;
        };
        let end = record.ended_at_ms.map_or(start, |at| at.get());
        start <= self.end_ms && end >= self.start_ms
    }
}

/// When the trajectory reads newer records. Notifications arriving while a
/// read is scheduled share it; ones arriving while a read runs schedule
/// another afterwards, since that read may have missed what they announce.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RefreshGate {
    scheduled: bool,
    running: bool,
    again: bool,
}

impl RefreshGate {
    /// A notification arrived; returns whether to schedule a read.
    pub fn request(&mut self) -> bool {
        if self.running {
            self.again = true;
            return false;
        }
        !std::mem::replace(&mut self.scheduled, true)
    }

    /// The scheduled read starts.
    pub fn start(&mut self) {
        self.scheduled = false;
        self.running = true;
        self.again = false;
    }

    /// The read finished; returns whether to read again.
    pub fn finish(&mut self) -> bool {
        self.running = false;
        std::mem::take(&mut self.again)
    }

    /// Whether a read is running.
    pub fn running(&self) -> bool {
        self.running
    }
}

/// How a page changed the loaded records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageApplied {
    /// Everything loaded before was dropped.
    Replaced,
    /// The page went before the loaded records.
    Prepended,
    /// The page went after them, replacing records it carries again.
    Appended,
}

/// Totals over the loaded records.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, TypedBuilder)]
pub struct TrajectorySummary {
    /// Runs seen.
    pub runs: usize,
    /// Model requests.
    pub requests: usize,
    /// Input tokens.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Cache-read share of the input, rounded; `None` without input.
    #[builder(default)]
    pub cache_hit_percent: Option<u8>,
}

/// One group of consecutive records: a Run, or the records between Runs.
struct RunGroup<'a> {
    key: RunKey,
    index: Option<u32>,
    records: &'a [TrajectoryRecord],
}

impl RunGroup<'_> {
    /// The title row of the group.
    fn header(&self, collapsed: bool) -> LedgerRow {
        let starts = self
            .records
            .iter()
            .filter_map(|record| record.started_at_ms.map(|at| at.get()));
        let ends = self.records.iter().filter_map(|record| {
            record
                .ended_at_ms
                .or(record.started_at_ms)
                .map(|at| at.get())
        });
        let duration_ms = starts
            .min()
            .zip(ends.max())
            .map(|(start, end)| end.saturating_sub(start));
        let tokens = self
            .records
            .iter()
            .filter_map(|record| match &record.kind {
                TrajectoryRecordKind::Assistant(assistant) => {
                    let usage = assistant.usage.as_ref();
                    Some(
                        usage.input_tokens.unwrap_or_default()
                            + usage.output_tokens.unwrap_or_default(),
                    )
                }
                _ => None,
            })
            .sum();
        LedgerRow::RunHeader {
            key: self.key.clone(),
            index: self.index,
            duration_ms,
            tokens,
            collapsed,
        }
    }
}

/// Lookups derived from the loaded records, rebuilt whenever they change,
/// so per-frame reads never scan the records.
#[derive(Debug, Clone, Default)]
struct RecordIndex {
    /// Position of each record by id.
    positions: HashMap<String, usize>,
    /// Position of each request record by request number.
    requests: HashMap<u32, usize>,
    /// Usage of the loaded requests up to each request number.
    usage: BTreeMap<u32, ModelUsage>,
    /// Totals over the loaded records.
    summary: TrajectorySummary,
    /// The newest time any loaded record reached.
    last_ended_ms: Option<u64>,
}

impl From<&[TrajectoryRecord]> for RecordIndex {
    /// Indexes `records` in one pass.
    fn from(records: &[TrajectoryRecord]) -> Self {
        let mut index = Self::default();
        let mut runs = HashSet::new();
        let (mut requests, mut input, mut output, mut cached) = (0, 0, 0, 0);
        let mut total = ModelUsage::default();
        for (position, record) in records.iter().enumerate() {
            index.positions.insert(record.id.clone(), position);
            if let Some(run) = &record.run {
                runs.insert(&run.id);
            }
            let ended = record.ended_at_ms.or(record.started_at_ms);
            index.last_ended_ms =
                index.last_ended_ms.max(ended.map(|at| at.get()));
            match &record.kind {
                TrajectoryRecordKind::Request(request) => {
                    index.requests.insert(request.index, position);
                }
                TrajectoryRecordKind::Assistant(assistant) => {
                    let usage = assistant.usage.as_ref();
                    requests += 1;
                    input += usage.input_tokens.unwrap_or_default();
                    output += usage.output_tokens.unwrap_or_default();
                    cached += usage.cached_input_tokens.unwrap_or_default();
                    total += &assistant.usage;
                    index.usage.insert(assistant.request_index, total);
                }
                _ => {}
            }
        }
        let cache_hit_percent = (input > 0).then(|| {
            u8::try_from((cached * 100 + input / 2) / input).unwrap_or(100)
        });
        index.summary = TrajectorySummary::builder()
            .runs(runs.len())
            .requests(requests)
            .input(input)
            .output(output)
            .cache_hit_percent(cache_hit_percent)
            .build();
        index
    }
}

/// The loaded records of one Session's trajectory and the ledger's view state.
#[derive(Debug, Clone, TypedBuilder)]
pub struct TrajectoryModel {
    #[builder(default)]
    records: Vec<TrajectoryRecord>,
    #[builder(default)]
    leaf_id: Option<EntryId>,
    #[builder(default)]
    has_more_before: bool,
    /// Cursor of the next earlier page.
    #[builder(default)]
    first_sequence: Option<u64>,
    /// Cursor of the next refresh.
    #[builder(default)]
    last_sequence: Option<u64>,
    #[builder(default)]
    usage_before: ModelUsage,
    #[builder(default)]
    selected: Option<String>,
    #[builder(default)]
    collapsed_runs: HashSet<RunKey>,
    #[builder(default)]
    calls_folded: bool,
    /// Requests whose calls stay unfolded while calls are folded.
    #[builder(default)]
    expanded_calls: HashSet<u32>,
    #[builder(default)]
    interval: Option<TimeInterval>,
    #[builder(default, setter(skip))]
    index: RecordIndex,
    /// Counts changes to the loaded records, for caches derived from them.
    #[builder(default, setter(skip))]
    revision: u64,
}

impl Default for TrajectoryModel {
    /// Nothing loaded, nothing folded or filtered.
    fn default() -> Self {
        Self::builder().build()
    }
}

impl TrajectoryModel {
    /// Folds in a page read at `cursor`. A tail page, or the tail of a branch
    /// that replaced the loaded one, replaces everything; the selection stays
    /// when its record does.
    pub fn apply(
        &mut self,
        cursor: TrajectoryCursor,
        page: TrajectoryPage,
    ) -> PageApplied {
        let replaced =
            matches!(cursor, TrajectoryCursor::Tail) || page.branch_changed;
        let applied = if replaced {
            self.records = page.records;
            self.first_sequence = page.first_sequence;
            self.last_sequence = page.last_sequence;
            self.has_more_before = page.has_more_before;
            self.usage_before = page.usage_before;
            PageApplied::Replaced
        } else if matches!(cursor, TrajectoryCursor::Before(_)) {
            self.merge(page.records, true);
            self.first_sequence = page.first_sequence.or(self.first_sequence);
            self.has_more_before = page.has_more_before;
            self.usage_before = page.usage_before;
            PageApplied::Prepended
        } else {
            self.merge(page.records, false);
            self.last_sequence = page.last_sequence.or(self.last_sequence);
            PageApplied::Appended
        };
        self.leaf_id = page.leaf_id;
        self.index = RecordIndex::from(self.records.as_slice());
        self.revision += 1;
        if let Some(id) = &self.selected
            && !self.index.positions.contains_key(id)
        {
            self.selected = None;
        }
        applied
    }

    /// Counts changes to the loaded records.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The loaded record `id`.
    pub fn record(&self, id: &str) -> Option<&TrajectoryRecord> {
        self.records.get(*self.index.positions.get(id)?)
    }

    /// The session's usage up to and including request `index`: what came
    /// before the loaded records plus the loaded requests through `index`.
    pub fn usage_through(&self, index: u32) -> ModelUsage {
        let mut usage = self.usage_before;
        if let Some((_, loaded)) = self.index.usage.range(..=index).next_back()
        {
            usage += loaded;
        }
        usage
    }

    /// Replaces records the page carries again and puts the rest before or
    /// after the loaded ones.
    fn merge(&mut self, incoming: Vec<TrajectoryRecord>, front: bool) {
        let positions: HashMap<String, usize> = self
            .records
            .iter()
            .enumerate()
            .map(|(position, record)| (record.id.clone(), position))
            .collect();
        let mut fresh = Vec::with_capacity(incoming.len());
        for record in incoming {
            match positions.get(&record.id) {
                Some(&position) => {
                    if let Some(slot) = self.records.get_mut(position) {
                        *slot = record;
                    }
                }
                None => fresh.push(record),
            }
        }
        if front {
            fresh.append(&mut self.records);
            self.records = fresh;
        } else {
            self.records.append(&mut fresh);
        }
    }

    /// The loaded records, in branch order.
    pub fn records(&self) -> &[TrajectoryRecord] {
        &self.records
    }

    /// The branch leaf the records belong to.
    pub fn leaf_id(&self) -> Option<&EntryId> {
        self.leaf_id.as_ref()
    }

    /// Whether earlier records exist.
    pub fn has_more_before(&self) -> bool {
        self.has_more_before
    }

    /// Cursor of the next earlier page.
    pub fn first_sequence(&self) -> Option<u64> {
        self.first_sequence
    }

    /// Cursor of the next refresh.
    pub fn last_sequence(&self) -> Option<u64> {
        self.last_sequence
    }

    /// Usage of the requests before the loaded records.
    pub fn usage_before(&self) -> &ModelUsage {
        &self.usage_before
    }

    /// The newest time any loaded record reached.
    pub fn last_ended_ms(&self) -> Option<u64> {
        self.index.last_ended_ms
    }

    /// The groups of consecutive records sharing a Run.
    fn groups(&self) -> impl Iterator<Item = RunGroup<'_>> {
        self.records
            .chunk_by(|a, b| {
                a.run.as_ref().map(|run| &run.id)
                    == b.run.as_ref().map(|run| &run.id)
            })
            .filter_map(|records| {
                let first = records.first()?;
                Some(match &first.run {
                    Some(run) => RunGroup {
                        key: RunKey::Run(run.id.clone()),
                        index: Some(run.index),
                        records,
                    },
                    None => RunGroup {
                        key: RunKey::Between(first.sequence),
                        index: None,
                        records,
                    },
                })
            })
    }

    /// Whether a time interval narrows the rows.
    pub fn is_filtering(&self) -> bool {
        self.interval.is_some()
    }

    /// Whether `record` falls in the interval.
    fn shows(&self, record: &TrajectoryRecord) -> bool {
        self.interval
            .is_none_or(|interval| interval.overlaps(record))
    }

    /// The ledger rows, ending with `in_flight`.
    pub fn rows(&self, in_flight: &[InFlight]) -> Vec<LedgerRow> {
        let mut rows = Vec::new();
        if self.has_more_before {
            rows.push(LedgerRow::LoadEarlier);
        }
        let filtering = self.is_filtering();
        for group in self.groups() {
            let visible: Vec<&TrajectoryRecord> = group
                .records
                .iter()
                .filter(|record| self.shows(record))
                .collect();
            if visible.is_empty() {
                continue;
            }
            let collapsed = self.collapsed_runs.contains(&group.key);
            rows.push(group.header(collapsed));
            if collapsed {
                continue;
            }
            let mut request = None;
            for record in visible {
                match &record.kind {
                    TrajectoryRecordKind::Request(r) => request = Some(r.index),
                    TrajectoryRecordKind::Assistant(a) => {
                        request = Some(a.request_index);
                    }
                    TrajectoryRecordKind::Tool(tool)
                        if self.calls_folded && !filtering =>
                    {
                        if let Some(index) = request
                            && !self.expanded_calls.contains(&index)
                        {
                            Self::fold_call(&mut rows, index, record, tool);
                            continue;
                        }
                    }
                    _ => {}
                }
                rows.push(LedgerRow::Record {
                    id: record.id.clone(),
                });
            }
        }
        // A call whose record is loaded shows as that record.
        rows.extend(
            in_flight
                .iter()
                .filter(|work| {
                    work.record_id().is_none_or(|id| self.record(&id).is_none())
                })
                .cloned()
                .map(LedgerRow::InFlight),
        );
        rows
    }

    /// Adds one call to the request's fold, starting it when needed.
    fn fold_call(
        rows: &mut Vec<LedgerRow>,
        index: u32,
        record: &TrajectoryRecord,
        tool: &protocol::ToolRecord,
    ) {
        let duration = record
            .started_at_ms
            .zip(record.ended_at_ms)
            .map_or(0, |(start, end)| end.get().saturating_sub(start.get()));
        let failed = usize::from(tool.is_error);
        if let Some(LedgerRow::CallGroup {
            request_index,
            count,
            duration_ms,
            failures,
        }) = rows.last_mut()
            && *request_index == index
        {
            *count += 1;
            *duration_ms += duration;
            *failures += failed;
            return;
        }
        rows.push(LedgerRow::CallGroup {
            request_index: index,
            count: 1,
            duration_ms: duration,
            failures: failed,
        });
    }

    /// Selects the record `id`.
    pub fn select(&mut self, id: &str) {
        // Only loaded records can be selected; a running request's bar, for
        // one, leaves the selection as it was.
        if self.record(id).is_some() {
            self.selected = Some(id.to_owned());
        }
    }

    /// The selected record, when it is loaded.
    pub fn selected_record(&self) -> Option<&TrajectoryRecord> {
        self.record(self.selected.as_deref()?)
    }

    /// Moves the selection `delta` record rows among `rows`, the rows shown,
    /// stopping at the ends; with nothing selected, selects the first or last
    /// record row.
    pub fn select_adjacent<'a>(
        &mut self,
        rows: impl IntoIterator<Item = &'a LedgerRow>,
        delta: isize,
    ) -> Option<&TrajectoryRecord> {
        let ids: Vec<&str> = rows
            .into_iter()
            .filter_map(|row| match row {
                LedgerRow::Record { id } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        let last = ids.len().checked_sub(1)?;
        let current = self
            .selected
            .as_deref()
            .and_then(|selected| ids.iter().position(|id| *id == selected));
        let next = match current {
            Some(position) => position.saturating_add_signed(delta).min(last),
            None if delta < 0 => last,
            None => 0,
        };
        self.selected = ids.get(next).map(|id| (*id).to_owned());
        self.selected_record()
    }

    /// Folds or unfolds one group.
    pub fn toggle_run(&mut self, key: RunKey) {
        if !self.collapsed_runs.remove(&key) {
            self.collapsed_runs.insert(key);
        }
    }

    /// Folds or unfolds every group.
    pub fn set_all_runs_collapsed(&mut self, collapsed: bool) {
        self.collapsed_runs = if collapsed {
            self.groups().map(|group| group.key).collect()
        } else {
            HashSet::new()
        };
    }

    /// Whether every group is folded; `false` with none loaded.
    pub fn all_runs_collapsed(&self) -> bool {
        let mut groups = self.groups().peekable();
        // With nothing loaded there is nothing collapsed.
        groups.peek().is_some()
            && groups.all(|group| self.collapsed_runs.contains(&group.key))
    }

    /// Folds each request's tool calls into one row, or shows them all.
    pub fn set_calls_folded(&mut self, folded: bool) {
        self.calls_folded = folded;
        self.expanded_calls.clear();
    }

    /// Whether tool calls are folded.
    pub fn calls_folded(&self) -> bool {
        self.calls_folded
    }

    /// Unfolds or refolds the calls of one request.
    pub fn toggle_call_group(&mut self, request_index: u32) {
        if !self.expanded_calls.remove(&request_index) {
            self.expanded_calls.insert(request_index);
        }
    }

    /// Keeps only records meeting `interval`.
    pub fn set_interval(&mut self, interval: Option<TimeInterval>) {
        self.interval = interval;
    }

    /// The time interval filter.
    pub fn interval(&self) -> Option<TimeInterval> {
        self.interval
    }

    /// Totals over the loaded records.
    pub fn summary(&self) -> TrajectorySummary {
        self.index.summary
    }

    /// The loaded request before request `index`, for comparisons.
    pub fn previous_request(&self, index: u32) -> Option<&RequestRecord> {
        let position = *self.index.requests.get(&index.checked_sub(1)?)?;
        match &self.records.get(position)?.kind {
            TrajectoryRecordKind::Request(request) => Some(request),
            _ => None,
        }
    }
}
