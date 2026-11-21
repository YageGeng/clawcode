//! The session details card: inline or in a popover, its rows, the rows the
//! user hides, and how it steps down when the card is short.

mod support;

use std::path::PathBuf;

use desktop::state::{DesktopState, DetailsItem, PanelTabKey};
use desktop::ui::{DetailsDensity, TabEntry};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{SharedString, TestAppContext, px, size};
use support::shell::{
    Live, eventually, live, live_in_git_repo, session, shows,
};

/// The fixture workspace of a live shell.
fn root(live: &Live) -> PathBuf {
    live.backend.workspace.path().to_path_buf()
}

/// A live Git shell with its fixture workspace registered and one session open.
fn git_session(cx: &mut TestAppContext) -> Live {
    let live = live_in_git_repo(cx);
    live.add_workspace(cx, &root(&live));
    live.open_new_session(cx);
    live
}

/// Clicks an element by id in a fresh frame.
fn click(live: &Live, cx: &mut TestAppContext, id: &str) {
    let id = SharedString::from(id.to_string());
    live.window(cx, |window, cx| window.click(id, cx));
}

#[test]
fn density_steps_down_and_scrolls_last() {
    let heights = [px(300.), px(200.), px(80.)];
    assert_eq!(
        DetailsDensity::fit(px(400.), heights),
        (DetailsDensity::Full, false)
    );
    assert_eq!(
        DetailsDensity::fit(px(250.), heights),
        (DetailsDensity::Compact, false)
    );
    assert_eq!(
        DetailsDensity::fit(px(100.), heights),
        (DetailsDensity::Essential, false)
    );
    assert_eq!(
        DetailsDensity::fit(px(50.), heights),
        (DetailsDensity::Essential, true)
    );
}

#[gpui_kit::test]
fn details_card_is_inline_when_wide_and_a_popover_when_narrow(
    cx: &mut TestAppContext,
) {
    let live = git_session(cx);
    live.until(cx, "inline card", |window| {
        window.try_find("details-branch").is_some()
    });
    live.window(cx, |window, _| {
        assert!(window.try_find("details-card").is_some())
    });

    cx.simulate_window_resize(live.handle, size(px(1000.), px(820.)));
    live.window(cx, |window, _| {
        assert!(window.try_find("details-card").is_none())
    });
    click(&live, cx, "header-details");
    live.until(cx, "popover", |window| {
        window.try_find("details-branch").is_some()
    });
}

#[gpui_kit::test]
fn header_details_hides_and_shows_the_inline_card(cx: &mut TestAppContext) {
    let live = git_session(cx);
    live.until(cx, "inline card", |window| {
        window.try_find("details-card").is_some()
    });
    click(&live, cx, "header-details");
    live.window(cx, |window, _| {
        assert!(window.try_find("details-card").is_none())
    });
    cx.update(|cx| assert!(!live.view.read(cx).state().layout.details_open));
    click(&live, cx, "header-details");
    live.window(cx, |window, _| {
        assert!(window.try_find("details-card").is_some())
    });
}

#[gpui_kit::test]
fn changes_row_opens_the_changes_tab(cx: &mut TestAppContext) {
    let live = git_session(cx);
    std::fs::write(root(&live).join("a.rs"), "changed\n").expect("edit");
    live.view
        .update(cx, |view, cx| view.refresh_git(root(&live), cx));
    live.until(cx, "changes row", |window| {
        window.try_find("details-changes").is_some()
    });
    click(&live, cx, "details-changes");
    cx.update(|cx| {
        let panel = live.view.read(cx).panel().expect("panel");
        assert!(panel.open);
        assert_eq!(
            panel.active.and_then(|index| panel.tabs.get(index)),
            Some(&PanelTabKey::Changes)
        );
    });
}

#[gpui_kit::test]
fn hidden_items_disappear_and_only_changes_are_stored(cx: &mut TestAppContext) {
    let live = git_session(cx);
    live.until(cx, "branch row", |window| {
        window.try_find("details-branch").is_some()
    });
    live.view.update(cx, |view, cx| {
        view.set_details_item(DetailsItem::Branch, false, cx)
    });
    live.window(cx, |window, _| {
        assert!(window.try_find("details-branch").is_none())
    });
    cx.update(|cx| {
        let items = &live.view.read(cx).state().details_items;
        assert_eq!(items.len(), 1);
        assert_eq!(items.get(&DetailsItem::Branch), Some(&false));
    });
}

#[gpui_kit::test]
fn context_and_usage_are_hidden_by_default(cx: &mut TestAppContext) {
    let live = git_session(cx);
    live.until(cx, "card", |window| {
        window.try_find("details-card").is_some()
    });
    live.window(cx, |window, _| {
        assert!(window.try_find("details-context").is_none());
        assert!(window.try_find("details-usage").is_none());
    });
    live.view.update(cx, |view, cx| {
        view.set_details_item(DetailsItem::Context, true, cx)
    });
    live.window(cx, |window, _| {
        assert!(window.try_find("details-context").is_some())
    });
}

#[gpui_kit::test]
fn non_repository_hides_version_control(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    live.add_workspace(cx, &root(&live));
    live.open_new_session(cx);
    eventually(cx, "status read", |cx| {
        live.view.read(cx).is_repository(&root(&live)) == Some(false)
    });
    live.window(cx, |window, _| {
        assert!(window.try_find("details-card").is_some());
        assert!(window.try_find("details-branch").is_none());
        assert!(window.try_find("details-changes").is_none());
        assert_eq!(window.find("header-changes").disabled(), Some(true));
    });
}

#[gpui_kit::test]
fn relations_group_folds_previous_agents(cx: &mut TestAppContext) {
    // No backend: the catalog stays as given, and no subagent is seen running.
    let (handle, view, _dir) =
        support::shell::open(cx, support::shell::with_workspaces(&["/w/a"]));
    let child = |name: &str, modified: u64| {
        let mut summary = session(name, "/w/a", Some(name), modified);
        summary.parent_session_id = Some(support::shell::id("p1"));
        summary
    };
    view.update(cx, |view, cx| {
        view.replace_catalog(
            [
                session("p1", "/w/a", Some("parent"), 100),
                child("c1", 50),
                child("c2", 60),
            ]
            .into_iter()
            .collect(),
            cx,
        );
        view.open_session(&support::shell::id("p1"), cx);
    });
    let frame = |cx: &mut TestAppContext,
                 check: &dyn Fn(&mut gpui_kit::Window)| {
        gpui_kit::AppContext::update_window(cx, handle, |_, window, cx| {
            window.render_frame(cx);
            check(window);
        })
        .expect("window");
    };
    frame(cx, &|window| {
        assert!(shows(window, "Previous agents · 2"));
        assert!(
            window.try_find("details-agent-c1").is_none(),
            "previous agents start folded"
        );
    });
    gpui_kit::AppContext::update_window(cx, handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(SharedString::from("details-previous"), cx);
    })
    .expect("window");
    frame(cx, &|window| {
        assert!(window.try_find("details-agent-c1").is_some());
        assert!(window.try_find("details-agent-c2").is_some());
    });
}

#[gpui_kit::test]
fn a_subagent_shows_its_parent_row(cx: &mut TestAppContext) {
    let live = git_session(cx);
    let parent = cx.update(|cx| match live.view.read(cx).tabs() {
        [TabEntry::Session(id)] => id.clone(),
        other => panic!("tabs {other:?}"),
    });
    let child = live.backend.new_session();
    live.view.update(cx, |view, cx| {
        let mut catalog = view.catalog().clone();
        let mut summary = session(
            &child.to_string(),
            root(&live).to_str().expect("utf8"),
            None,
            5,
        );
        summary.parent_session_id = Some(parent.clone());
        catalog.upsert(summary);
        view.replace_catalog(catalog, cx);
        view.open_session(&child, cx);
    });
    live.until(cx, "parent row", |window| {
        window.try_find("details-parent").is_some()
    });
    click(&live, cx, "details-parent");
    cx.update(|cx| {
        assert_eq!(
            live.view.read(cx).active_entry(),
            Some(TabEntry::Session(parent.clone()))
        )
    });
}

#[gpui_kit::test]
fn details_explain_missing_workspaces(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    live.view.update(cx, |view, cx| view.open_draft(None, cx));
    live.until(cx, "empty card", |window| {
        window.try_find("details-add-workspace").is_some()
    });

    let gone = tempfile::tempdir().expect("gone");
    let path = gone.path().to_path_buf();
    live.add_workspace(cx, &path);
    live.view
        .update(cx, |view, cx| view.open_draft(Some(path.clone()), cx));
    drop(gone);
    live.view.update(cx, |view, cx| view.refresh_catalog(cx));
    live.until(cx, "missing note", |window| {
        shows(window, "Folder not found")
    });
}

#[gpui_kit::test]
fn subagent_row_opens_in_the_panel_by_default_and_main_with_ctrl(
    cx: &mut TestAppContext,
) {
    let (handle, view, _dir) =
        support::shell::open(cx, support::shell::with_workspaces(&["/w/a"]));
    let mut child = session("c1", "/w/a", Some("child"), 50);
    child.parent_session_id = Some(support::shell::id("p1"));
    view.update(cx, |view, cx| {
        view.replace_catalog(
            [session("p1", "/w/a", Some("parent"), 100), child]
                .into_iter()
                .collect(),
            cx,
        );
        view.open_session(&support::shell::id("p1"), cx);
    });
    let click =
        |cx: &mut TestAppContext, id: &str, modifiers: gpui_kit::Modifiers| {
            let id = SharedString::from(id.to_string());
            gpui_kit::AppContext::update_window(cx, handle, |_, window, cx| {
                window.render_frame(cx);
                window.click_with_modifiers(id, modifiers, cx);
            })
            .expect("window");
        };
    click(cx, "details-previous", gpui_kit::Modifiers::default());
    click(cx, "details-agent-c1", gpui_kit::Modifiers::default());
    cx.update(|cx| {
        let view = view.read(cx);
        let panel = view.panel().expect("panel");
        assert!(panel.open);
        assert_eq!(
            panel.active.and_then(|index| panel.tabs.get(index)),
            Some(&PanelTabKey::Subagent {
                session: "c1".to_string()
            })
        );
        assert_eq!(view.tabs().len(), 1, "the main card keeps its tabs");
    });

    // Ctrl+click takes the other place: a session tab in the main card.
    // (The open panel leaves no room for the inline card, so it is closed first.)
    gpui_kit::AppContext::update_window(cx, handle, |_, window, cx| {
        view.update(cx, |view, cx| view.toggle_panel(window, cx))
    })
    .expect("window");
    click(cx, "details-agent-c1", gpui_kit::Modifiers::control());
    cx.update(|cx| {
        assert!(
            view.read(cx)
                .tabs()
                .contains(&TabEntry::Session(support::shell::id("c1")))
        )
    });
}

#[gpui_kit::test]
fn subagent_target_main_opens_plain_clicks_in_the_main_card(
    cx: &mut TestAppContext,
) {
    let mut state = support::shell::with_workspaces(&["/w/a"]);
    state.subagent_open_target = desktop::state::SubagentOpenTarget::Main;
    let (handle, view, _dir) = support::shell::open(cx, state);
    let mut child = session("c1", "/w/a", Some("child"), 50);
    child.parent_session_id = Some(support::shell::id("p1"));
    view.update(cx, |view, cx| {
        view.replace_catalog(
            [session("p1", "/w/a", Some("parent"), 100), child]
                .into_iter()
                .collect(),
            cx,
        );
        view.open_session(&support::shell::id("p1"), cx);
    });
    gpui_kit::AppContext::update_window(cx, handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(SharedString::from("details-previous"), cx);
        window.render_frame(cx);
        window.click(SharedString::from("details-agent-c1"), cx);
    })
    .expect("window");
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(
            view.active_entry(),
            Some(TabEntry::Session(support::shell::id("c1")))
        );
    });
}

#[gpui_kit::test]
fn the_inline_card_leaves_the_transcript_uncovered(cx: &mut TestAppContext) {
    let live = git_session(cx);
    let id = cx.update(|cx| match live.view.read(cx).tabs() {
        [TabEntry::Session(id)] => id.clone(),
        other => panic!("tabs {other:?}"),
    });
    live.run(cx, &id, vec!["hello".into()]);
    live.until(cx, "reply", |window| shows(window, "Hi there"));
    live.until(cx, "card", |window| {
        window.try_find("details-card").is_some()
    });
    let message = cx.update(|cx| {
        let handle = live.session(&id, cx).expect("attached");
        let projection = handle.read(cx).projection();
        match projection.row(0) {
            Some(desktop::projection::TranscriptRow::Message(message)) => {
                SharedString::from(format!("message:{}", message.message_id))
            }
            other => panic!("unexpected row {other:?}"),
        }
    });
    live.window(cx, |window, _| {
        let card = window.find("details-card").bounds();
        let row = window.find(message.clone()).bounds();
        assert!(
            row.right() <= card.left(),
            "the user message {row:?} runs under the card {card:?}"
        );
    });
}
