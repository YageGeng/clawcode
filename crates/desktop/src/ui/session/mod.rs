//! One tab's session view: the transcript in a `MessageScroller`, its render caches, and the composer slot.

mod message_actions;
pub(in crate::ui) mod rows;
mod turn_changes;

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use agent_client_protocol::v2::schema as wire;
use base64::Engine as _;
use gpui_kit::base::actions::Cancel;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::diff::{DiffFile, DiffState};
use gpui_kit::component::input::{InputEvent, TextareaState};
use gpui_kit::component::label::Label;
use gpui_kit::component::message_scroller::{
    MessageScroller, MessageScrollerState,
};
use gpui_kit::component::skeleton::Skeleton;
use gpui_kit::component::text::TextViewState;
use gpui_kit::component::{ResizableState, resizable_panel, v_resizable};
use gpui_kit::{
    App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, Image,
    ImageFormat, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, StyleRefinement, Styled as _, Subscription, Task, Window, div,
};
use typed_builder::TypedBuilder;

use crate::backend::AcpClient;
use crate::projection::{
    MessageOrigin, ProjectionChange, RowKey, ToolCategory, TranscriptRow,
};
use crate::session::{AttachPhase, SessionHandle};
use crate::transcript::{LayoutRow, TranscriptLayout};
use crate::ui::composer::Composer;
use crate::ui::panel::RecordView;
use crate::ui::trajectory::{TrajectoryEvent, TrajectoryView};

/// What a tab shows: a draft that has no backend session yet, or an attached session.
pub enum SessionTarget {
    /// A new session not created in the backend yet.
    Draft {
        /// Workspace the session will run in.
        workspace: Option<PathBuf>,
    },
    /// An attached session.
    Session(Entity<SessionHandle>),
}

/// What a session tab's body shows above the composer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewMode {
    /// The conversation transcript.
    #[default]
    Chat,
    /// The trajectory of the agent loop.
    Trajectory,
}

/// Which text of a message a Markdown document renders.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MarkdownPart {
    /// The visible answer or prompt.
    Body,
    /// The assistant's thinking.
    Thinking,
}

/// Retained Markdown documents, one per message part, so streamed text is appended instead of re-parsed.
#[derive(Default)]
pub struct MarkdownCache {
    entries: HashMap<(wire::MessageId, MarkdownPart), Entity<TextViewState>>,
}

impl MarkdownCache {
    /// Returns the document for one message part, created on first sight and updated to `text`.
    ///
    /// `TextViewState::set_text` appends with `push_str` when `text` extends what
    /// the document holds and replaces it otherwise, so the prefix check lives in
    /// one place and needs no third copy of the text here.
    pub fn sync(
        &mut self,
        id: &wire::MessageId,
        part: MarkdownPart,
        text: &str,
        cx: &mut App,
    ) -> Entity<TextViewState> {
        match self.entries.entry((id.clone(), part)) {
            Entry::Occupied(entry) => {
                let state = entry.get().clone();
                state.update(cx, |state, cx| state.set_text(text, cx));
                state
            }
            Entry::Vacant(entry) => entry
                .insert(cx.new(|cx| TextViewState::markdown(text, cx)))
                .clone(),
        }
    }

    /// The document for one message part, if it was synced.
    pub fn get(
        &self,
        id: &wire::MessageId,
        part: MarkdownPart,
    ) -> Option<&Entity<TextViewState>> {
        self.entries.get(&(id.clone(), part))
    }
}

/// What a session view asks the shell to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionViewEvent {
    /// A draft's first message: create the session, then send `message`.
    CreateSession {
        /// The message to send once the session exists.
        message: crate::ui::composer::attachments::Outgoing,
    },
    /// The draft should run in another registered workspace.
    SelectWorkspace(PathBuf),
    /// Something the tab's trajectory asks of the shell.
    Trajectory(TrajectoryEvent),
    /// A reply was forked into this new session, which should open in a tab.
    Forked(crate::backend::SessionId),
    /// A turn's change card asked to show what `run` changed, and `path`
    /// within it, in the panel's Changes tab.
    OpenChanges {
        /// The run whose changes to show.
        run: protocol::RunId,
        /// The file to bring into view.
        path: Option<String>,
    },
}

/// Loading placeholder shown while a session's transcript is attaching.
pub(super) fn loading_skeleton() -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_6()
        .child(Skeleton::new().h_4().w_1_2())
        .child(Skeleton::new().h_4().w_3_4())
        .child(Skeleton::new().h_4().w_2_3())
}

/// One tab's content: the transcript of its session and the composer.
#[derive(TypedBuilder)]
pub struct SessionView {
    pub(super) target: SessionTarget,
    #[builder(default)]
    pub(super) layout: TranscriptLayout,
    scroller: Entity<MessageScrollerState>,
    #[builder(default)]
    pub(super) markdown: MarkdownCache,
    #[builder(default)]
    pub(super) diffs: HashMap<wire::ToolCallId, Entity<DiffState>>,
    #[builder(default)]
    pub(super) images: HashMap<(wire::MessageId, usize), Arc<Image>>,
    #[builder(default)]
    expanded_runs: HashSet<RowKey>,
    #[builder(default)]
    pub(super) open_thinking: HashSet<wire::MessageId>,
    #[builder(default)]
    pub(super) open_tools: HashSet<wire::ToolCallId>,
    #[builder(default)]
    pub(super) full_output: HashSet<wire::ToolCallId>,
    /// User-run commands shown with their full text.
    #[builder(default)]
    pub(super) open_commands: HashSet<wire::MessageId>,
    /// Compaction and branch-summary cards shown expanded.
    #[builder(default)]
    pub(super) open_cards: HashSet<RowKey>,
    /// Set while a configuration change waits for the agent; the bar's controls are disabled.
    #[builder(default)]
    pub(super) config_busy: bool,
    /// The options a new session starts with, which a draft offers.
    #[builder(default)]
    pub(super) draft_config: Vec<crate::projection::ConfigOption>,
    /// Choices made in a draft, by option id; applied once its session exists.
    #[builder(default)]
    pub(in crate::ui) draft_choices:
        std::collections::BTreeMap<&'static str, String>,
    /// The read of `draft_config` in flight.
    #[builder(default)]
    _draft_config_read: Option<Task<()>>,
    /// Choices made in the question panel, per question.
    #[builder(default = crate::ui::interactions::questions::QuestionDraft::builder().build())]
    pub(super) question: crate::ui::interactions::questions::QuestionDraft,
    /// The question panel's free-text "Other" input.
    pub(super) question_input: Entity<TextareaState>,
    /// Keeps the Send button in step with the typed answer.
    _question_input_changes: Subscription,
    /// The plan panel's open state chosen by the user, with the automatic state it overrode.
    #[builder(default)]
    pub(super) plan_override: Option<(bool, bool)>,
    /// The elicitation whose form dialog is open.
    #[builder(default)]
    pub(super) form_request: Option<crate::backend::RequestId>,
    pub(super) composer: Entity<Composer>,
    /// The agent connection; `None` until the backend is ready.
    #[builder(default)]
    pub(super) client: Option<AcpClient>,
    /// Registered workspaces a draft can switch to.
    #[builder(default)]
    pub(super) workspaces: Vec<PathBuf>,
    /// Set while the shell creates this draft's session; further sends wait.
    #[builder(default)]
    pub(super) creating: bool,
    /// Kept across targets, unlike `_subscriptions`.
    _composer_events: Subscription,
    /// Redraws on scroller-only changes such as the jump button; kept across targets.
    _scroller_changes: Subscription,
    #[builder(default)]
    _subscriptions: Vec<Subscription>,
    /// Redraws when a diff finishes its background highlighting, one per entry of `diffs`.
    #[builder(default)]
    _diff_changes: HashMap<wire::ToolCallId, Subscription>,
    /// Space kept free at the right edge for the details card.
    #[builder(default)]
    right_inset: gpui_kit::Pixels,
    /// Whether messages offer copy only: a subagent shown in the panel
    /// neither branches nor forks (M4b spec §3.1).
    #[builder(default)]
    pub(super) copy_only: bool,
    /// Replies whose fork is in flight.
    #[builder(default)]
    pub(super) forking: HashSet<wire::MessageId>,
    /// Whether an edit is being branched and sent; other branch, fork, and
    /// send actions wait for it.
    #[builder(default)]
    pub(in crate::ui) branching: bool,
    /// Turn change cards shown with their files.
    #[builder(default)]
    pub(super) open_turn_changes: HashSet<protocol::RunId>,
    /// Which messages carry a Fork action or a turn's change card.
    #[builder(default)]
    pub(super) marks: message_actions::MessageMarks,
    /// Whether the body shows the chat or the trajectory; kept in memory only.
    #[builder(default)]
    view_mode: ViewMode,
    /// The trajectory, created the first time it shows.
    #[builder(default)]
    trajectory: Option<Entity<TrajectoryView>>,
    /// Passes the trajectory's requests on to the shell.
    #[builder(default)]
    _trajectory_events: Option<Subscription>,
    /// A record's detail shown below the trajectory, where no panel tab can
    /// hold it: in a subagent opened in the panel.
    #[builder(default, setter(skip))]
    detail: Option<InlineDetail>,
}

/// Starting height of the record detail below the trajectory, in rems.
const DETAIL_REMS: f32 = 22.0;

/// A record detail split below the trajectory.
struct InlineDetail {
    /// The detail, following the trajectory's selection.
    record: Entity<RecordView>,
    /// The split's sizes as the user drags its handle.
    split: Entity<ResizableState>,
    /// Closes the split when the detail's close button is pressed.
    _dismiss: Subscription,
}

impl EventEmitter<SessionViewEvent> for SessionView {}

impl SessionView {
    /// Keeps `inset` free at the right edge, so the centered transcript leaves room for the details card.
    pub fn set_right_inset(
        &mut self,
        inset: gpui_kit::Pixels,
        cx: &mut Context<'_, Self>,
    ) {
        if self.right_inset != inset {
            self.right_inset = inset;
            cx.notify();
        }
    }

    /// The usage of the last finished run shown in this transcript.
    pub fn last_run_usage(&self) -> Option<crate::transcript::RunUsage> {
        (0..self.layout.len())
            .rev()
            .find_map(|ix| match self.layout.row(ix) {
                Some(crate::transcript::LayoutRow::Usage { usage, .. }) => {
                    Some(usage.clone())
                }
                _ => None,
            })
    }

    /// Limits message actions to copy, for a subagent shown in the panel.
    pub(in crate::ui) fn set_copy_only(&mut self) {
        self.copy_only = true;
    }

    /// Creates the view and shows `target`; `client` sends its messages.
    pub fn new(
        target: SessionTarget,
        client: Option<AcpClient>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let composer = cx.new(|cx| Composer::new(window, cx));
        let composer_events =
            cx.subscribe_in(&composer, window, Self::on_composer_event);
        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
        let question_input =
            cx.new(|cx| TextareaState::new(window, cx).rows(2));
        let question_input_changes = cx
            .subscribe(&question_input, |_, _, _: &InputEvent, cx| cx.notify());
        // The shell renders this view cached, so entities it draws that are
        // not child views must notify it themselves.
        let scroller_changes = cx.observe(&scroller, |_, _, cx| cx.notify());
        let mut view = SessionView::builder()
            .target(SessionTarget::Draft { workspace: None })
            .scroller(scroller)
            .composer(composer)
            .question_input(question_input)
            ._question_input_changes(question_input_changes)
            .client(client)
            ._composer_events(composer_events)
            ._scroller_changes(scroller_changes)
            .build();
        view.set_target(target, window, cx);
        view
    }

    /// The message input.
    pub fn composer(&self) -> &Entity<Composer> {
        &self.composer
    }

    /// Takes the client once the backend is ready; nothing drawn depends on it.
    pub(super) fn set_client(
        &mut self,
        client: Option<&AcpClient>,
        cx: &mut Context<'_, Self>,
    ) {
        if self.client.is_none() {
            self.client = client.cloned();
            self.load_draft_config(cx);
        }
        if let Some(trajectory) = &self.trajectory {
            trajectory
                .update(cx, |trajectory, cx| trajectory.set_client(client, cx));
        }
    }

    /// Whether the body shows the chat or the trajectory.
    pub fn view_mode(&self) -> ViewMode {
        self.view_mode
    }

    /// Shows the chat or the trajectory; a draft has no trajectory. Showing
    /// the trajectory again reads what it missed while hidden.
    pub fn set_view_mode(
        &mut self,
        mode: ViewMode,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if mode == ViewMode::Trajectory {
            let existed = self.trajectory.is_some();
            let Some(trajectory) = self.trajectory(window, cx) else {
                return;
            };
            if existed {
                trajectory.update(cx, |trajectory, cx| trajectory.refresh(cx));
            }
        }
        self.view_mode = mode;
        cx.notify();
    }

    /// The trajectory, if it was shown.
    pub fn trajectory_view(&self) -> Option<&Entity<TrajectoryView>> {
        self.trajectory.as_ref()
    }

    /// The trajectory of the attached session, created on first use; `None`
    /// for a draft.
    pub fn trajectory(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<Entity<TrajectoryView>> {
        if let Some(trajectory) = &self.trajectory {
            return Some(trajectory.clone());
        }
        let handle = self.handle()?.clone();
        let client = self.client.clone();
        let trajectory =
            cx.new(|cx| TrajectoryView::new(handle, client, window, cx));
        self._trajectory_events = Some(cx.subscribe(
            &trajectory,
            |_, _, event: &TrajectoryEvent, cx| {
                cx.emit(SessionViewEvent::Trajectory(event.clone()))
            },
        ));
        self.trajectory = Some(trajectory.clone());
        Some(trajectory)
    }

    /// Shows the trajectory's selected record below it, once; the detail
    /// follows the selection from then on.
    pub fn open_detail(&mut self, cx: &mut Context<'_, Self>) {
        let Some(trajectory) = &self.trajectory else {
            return;
        };
        if self.detail.is_none() {
            let trajectory = trajectory.clone();
            let record =
                cx.new(|cx| RecordView::new(trajectory, cx).closable());
            let dismiss = cx
                .subscribe(&record, |this, _, _: &DismissEvent, cx| {
                    this.close_detail(cx)
                });
            self.detail = Some(InlineDetail {
                record,
                split: cx.new(|_| ResizableState::default()),
                _dismiss: dismiss,
            });
        }
        cx.notify();
    }

    /// Closes the record detail below the trajectory.
    pub fn close_detail(&mut self, cx: &mut Context<'_, Self>) {
        if self.detail.take().is_some() {
            cx.notify();
        }
    }

    /// Updates the workspaces a draft can switch to, redrawing when they changed.
    pub(super) fn set_workspaces(
        &mut self,
        workspaces: &[PathBuf],
        cx: &mut Context<'_, Self>,
    ) {
        if self.workspaces != workspaces {
            self.workspaces = workspaces.to_vec();
            cx.notify();
        }
    }

    /// The attached session this view shows, if any.
    pub fn handle(&self) -> Option<&Entity<SessionHandle>> {
        match &self.target {
            SessionTarget::Session(handle) => Some(handle),
            SessionTarget::Draft { .. } => None,
        }
    }

    /// Switches to another target, dropping every cache of the previous one.
    pub fn set_target(
        &mut self,
        target: SessionTarget,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let draft = matches!(target, SessionTarget::Draft { .. });
        self.composer
            .update(cx, |composer, cx| composer.set_draft(draft, window, cx));
        self.target = target;
        self.layout = TranscriptLayout::default();
        self.markdown = MarkdownCache::default();
        self.diffs.clear();
        self._diff_changes.clear();
        self.images.clear();
        self.expanded_runs.clear();
        self.open_thinking.clear();
        self.open_tools.clear();
        self.full_output.clear();
        self.open_commands.clear();
        self.open_cards.clear();
        self._subscriptions.clear();
        self.trajectory = None;
        self._trajectory_events = None;
        self.detail = None;
        self.view_mode = ViewMode::Chat;
        self.scroller
            .update(cx, |scroller, cx| scroller.reset(0, cx));
        if let SessionTarget::Session(handle) = &self.target {
            let handle = handle.clone();
            self._subscriptions = vec![
                cx.subscribe_in(
                    &handle,
                    window,
                    |this, _, change: &ProjectionChange, window, cx| {
                        // Inserted rows always land in the splice, so only
                        // updated rows can need a re-measure in place.
                        this.sync(&change.updated, change.structural, cx);
                        // Streamed text moves no turn boundary or run.
                        if change.structural
                            || change.run_state
                            || change.checkpoints
                        {
                            this.refresh_marks(cx);
                        }
                        // A withdrawn elicitation takes its open form with it.
                        if change.controls {
                            this.close_withdrawn_form(window, cx);
                            this.push_commands(cx);
                        }
                        // A hidden trajectory catches up when it shows again.
                        if this.view_mode == ViewMode::Trajectory
                            && let Some(trajectory) = &this.trajectory
                        {
                            trajectory.update(cx, |trajectory, cx| {
                                trajectory.session_changed(change, cx)
                            });
                        }
                    },
                ),
                // The attach phase changes without a projection event.
                cx.observe(&handle, |_, _, cx| cx.notify()),
            ];
            self.sync(&HashSet::new(), true, cx);
            self.push_commands(cx);
        }
        self.refresh_marks(cx);
        self.load_draft_config(cx);
        cx.notify();
    }

    /// Reads the options a new session starts with, once, for a draft.
    fn load_draft_config(&mut self, cx: &mut Context<'_, Self>) {
        if !matches!(self.target, SessionTarget::Draft { .. })
            || !self.draft_config.is_empty()
            || self._draft_config_read.is_some()
        {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        self._draft_config_read = Some(cx.spawn(async move |this, cx| {
            match client.default_config().await {
                Ok(options) => {
                    let _ = this.update(cx, |this, cx| {
                        this.draft_config = options
                            .iter()
                            .filter_map(|option| {
                                crate::projection::ConfigOption::try_from(
                                    option,
                                )
                                .ok()
                            })
                            .collect();
                        cx.notify();
                    });
                }
                Err(error) => {
                    tracing::warn!(
                        "failed to read the default config: {}",
                        error
                    );
                    // Allow another try when the view is shown again.
                    let _ = this
                        .update(cx, |this, _| this._draft_config_read = None);
                }
            }
        }));
    }

    /// Hands the session's slash commands to the composer's list.
    fn push_commands(&mut self, cx: &mut Context<'_, Self>) {
        let commands = self
            .handle()
            .map(|handle| {
                handle.read(cx).projection().controls().commands().to_vec()
            })
            .unwrap_or_default();
        self.composer
            .update(cx, |composer, cx| composer.set_commands(commands, cx));
    }

    /// Splices the scroller by the layout difference and refreshes the caches of changed rows.
    ///
    /// Only a `structural` change rebuilds the layout; otherwise the rows
    /// stay put and just the `updated` ones are re-measured.
    pub fn sync(
        &mut self,
        updated: &HashSet<RowKey>,
        structural: bool,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(handle) = self.handle().cloned() else {
            return;
        };
        let diff = if structural {
            let next = TranscriptLayout::build(
                handle.read(cx).projection(),
                &self.expanded_runs,
            );
            let diff = self.layout.diff(&next, updated);
            self.layout = next;
            diff
        } else {
            self.layout.content_diff(updated)
        };
        let mut touched = diff.remeasure.clone();
        if let Some((range, count)) = &diff.splice {
            touched.extend(range.start..range.start + count);
        }
        self.cache_rows(&handle, &touched, updated, cx);
        let len = self.layout.len();
        self.scroller.update(cx, |scroller, cx| {
            if let Some((range, count)) = diff.splice
                && !scroller.splice(range, count, cx)
            {
                // The scroller lost track of the layout; start it over rather than misplace rows.
                tracing::warn!(
                    "transcript scroller out of sync, resetting to {} rows",
                    len
                );
                scroller.reset(len, cx);
                return;
            }
            for ix in diff.remeasure {
                scroller.remeasure_items(ix..ix + 1, cx);
            }
        });
        cx.notify();
    }

    /// Re-measures every row, after a font size or language change.
    pub fn remeasure(&mut self, cx: &mut Context<'_, Self>) {
        self.scroller
            .update(cx, |scroller, cx| scroller.remeasure(cx));
    }

    /// Folds or unfolds a finished run's tools.
    pub fn toggle_run(&mut self, anchor: RowKey, cx: &mut Context<'_, Self>) {
        if !self.expanded_runs.remove(&anchor) {
            self.expanded_runs.insert(anchor);
        }
        self.sync(&HashSet::new(), true, cx);
    }

    /// Shows or hides a message's thinking text.
    pub fn toggle_thinking(
        &mut self,
        id: wire::MessageId,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.open_thinking.remove(&id) {
            self.open_thinking.insert(id.clone());
        }
        self.sync(&HashSet::from([RowKey::Message(id)]), false, cx);
    }

    /// Expands or collapses a compaction or branch-summary card.
    pub fn toggle_card(&mut self, key: RowKey, cx: &mut Context<'_, Self>) {
        if !self.open_cards.remove(&key) {
            self.open_cards.insert(key.clone());
        }
        self.sync(&HashSet::from([key]), false, cx);
    }

    /// Shows or hides a user-run command's full text.
    pub fn toggle_command(
        &mut self,
        id: wire::MessageId,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.open_commands.remove(&id) {
            self.open_commands.insert(id.clone());
        }
        self.sync(&HashSet::from([RowKey::Message(id)]), false, cx);
    }

    /// Expands or collapses a tool row's details.
    pub fn toggle_tool(
        &mut self,
        id: wire::ToolCallId,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.open_tools.remove(&id) {
            self.open_tools.insert(id.clone());
        }
        self.sync(&HashSet::from([RowKey::Tool(id)]), false, cx);
    }

    /// Switches an execute tool between its output tail and the full output.
    pub fn toggle_full_output(
        &mut self,
        id: wire::ToolCallId,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.full_output.remove(&id) {
            self.full_output.insert(id.clone());
        }
        self.sync(&HashSet::from([RowKey::Tool(id)]), false, cx);
    }

    /// Builds what `render_row` reads for the layout rows at `rows`: Markdown
    /// documents, decoded images, and parsed diffs of expanded edits.
    fn cache_rows(
        &mut self,
        handle: &Entity<SessionHandle>,
        rows: &[usize],
        updated: &HashSet<RowKey>,
        cx: &mut Context<'_, Self>,
    ) {
        let SessionView {
            layout,
            markdown,
            diffs,
            images,
            open_thinking,
            open_tools,
            _diff_changes: diff_changes,
            ..
        } = self;
        // Observing needs this view's context, which the handle update below lends out.
        let mut created = Vec::new();
        // Updating the handle lends its projection and an `App` that can create entities.
        handle.update(cx, |handle, cx| {
            let projection = handle.projection();
            for ix in rows {
                let Some(LayoutRow::Row { key, index, .. }) = layout.row(*ix)
                else {
                    continue;
                };
                match projection.row(*index) {
                    Some(TranscriptRow::Message(message)) => {
                        let id = &message.message_id;
                        // A user-run command renders as plain monospace text.
                        if !message.text.is_empty()
                            && message.origin != MessageOrigin::UserBash
                        {
                            markdown.sync(
                                id,
                                MarkdownPart::Body,
                                &message.text,
                                cx,
                            );
                        }
                        if !message.reasoning.is_empty()
                            && open_thinking.contains(id)
                        {
                            markdown.sync(
                                id,
                                MarkdownPart::Thinking,
                                &message.reasoning,
                                cx,
                            );
                        }
                        for (n, block) in message.images.iter().enumerate() {
                            if images.contains_key(&(id.clone(), n)) {
                                continue;
                            }
                            let format =
                                ImageFormat::from_mime_type(&block.mime_type);
                            let bytes =
                                base64::engine::general_purpose::STANDARD
                                    .decode(&block.data);
                            // An undecodable image is left out rather than shown broken.
                            if let (Some(format), Ok(bytes)) = (format, bytes) {
                                images.insert(
                                    (id.clone(), n),
                                    Arc::new(Image::from_bytes(format, bytes)),
                                );
                            }
                        }
                    }
                    Some(TranscriptRow::Tool(tool)) => {
                        let id = &tool.tool_call_id;
                        let wanted = tool.category() == ToolCategory::Edit
                            && open_tools.contains(id);
                        if !wanted
                            || (diffs.contains_key(id)
                                && !updated.contains(key))
                        {
                            continue;
                        }
                        match tool.patch.as_deref().map(DiffFile::parse) {
                            Some(Ok(files)) if !files.is_empty() => {
                                let state =
                                    cx.new(|cx| DiffState::new(files, cx));
                                diffs.insert(id.clone(), state.clone());
                                created.push((id.clone(), state));
                            }
                            // No patch or an unparsable one: the row falls back to monospace text.
                            _ => {
                                diffs.remove(id);
                                diff_changes.remove(id);
                            }
                        }
                    }
                    // Compaction summaries and error rows render plain text in M2 Task 5.
                    Some(
                        TranscriptRow::Compaction(_)
                        | TranscriptRow::Error(_)
                        | TranscriptRow::Mail(_),
                    )
                    | None => {}
                }
            }
        });
        for (id, state) in created {
            let observer = cx.observe(&state, |_, _, cx| cx.notify());
            self._diff_changes.insert(id, observer);
        }
    }

    /// The draft's empty state, shown until its first message creates the session.
    fn render_draft(
        &self,
        workspace: Option<&PathBuf>,
    ) -> gpui_kit::AnyElement {
        let name = workspace
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .p_8()
            .child(
                Label::new(
                    rust_i18n::t!("Card.new_session_prompt", workspace = name)
                        .into_owned(),
                )
                .text_2xl(),
            )
            .into_any_element()
    }
}

impl Render for SessionView {
    /// Shows the attach state, or the transcript followed by the composer.
    fn render(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        self.sync_question_input(window, cx);
        let body = match &self.target {
            SessionTarget::Draft { workspace } => {
                self.render_draft(workspace.as_ref())
            }
            SessionTarget::Session(handle) => {
                match handle.read(cx).phase().clone() {
                    AttachPhase::Attaching => {
                        loading_skeleton().into_any_element()
                    }
                    AttachPhase::Failed(error) => div()
                        .p_6()
                        .child(Label::new(error).text_color(cx.theme().danger))
                        .into_any_element(),
                    AttachPhase::Attached
                        if self.view_mode == ViewMode::Trajectory
                            && let Some(trajectory) = &self.trajectory =>
                    {
                        let body = div().flex_1().min_h_0();
                        match &self.detail {
                            None => body.child(trajectory.clone()),
                            // The ledger above, the detail below. Escape
                            // closes the detail first: the ledger binds it
                            // to `Cancel`, which is caught on the way down.
                            Some(detail) => body
                                .capture_action(cx.listener(
                                    |this, _: &Cancel, _, cx| {
                                        this.close_detail(cx);
                                        cx.stop_propagation();
                                    },
                                ))
                                .child(
                                    v_resizable("trajectory-detail-split")
                                        .with_state(&detail.split)
                                        .child(
                                            resizable_panel()
                                                .child(trajectory.clone()),
                                        )
                                        .child(
                                            resizable_panel()
                                                .size(
                                                    window.rem_size()
                                                        * DETAIL_REMS,
                                                )
                                                .child(detail.record.clone()),
                                        ),
                                ),
                        }
                        .into_any_element()
                    }
                    AttachPhase::Attached => {
                        let view = cx.entity();
                        MessageScroller::new(
                            "transcript",
                            self.scroller.clone(),
                            move |ix, window, cx| {
                                view.update(cx, |view, cx| {
                                    view.render_row(ix, window, cx)
                                })
                            },
                        )
                        .jump_button(true)
                        .with_jump_button_label(
                            rust_i18n::t!("Transcript.jump_latest")
                                .into_owned(),
                        )
                        .with_bottom_fade(cx.theme().background)
                        // The transcript column's outer margins ride on the rows (see MessageScroller).
                        .with_list_style(
                            StyleRefinement::default().pt_5().px_7(),
                        )
                        // Rows space themselves so consecutive tool rows can join into one box.
                        .with_row_style(StyleRefinement::default().pb_0())
                        .flex_1()
                        .min_h_0()
                        .into_any_element()
                    }
                }
            }
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            // Room for the details card floating at the card's right (M3b spec §5.1).
            .pr(self.right_inset)
            .child(body)
            .child(self.render_composer(cx))
    }
}
