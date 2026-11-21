//! Tab strip rules, exercised without a window.

use std::path::PathBuf;

use desktop::backend::SessionId;
use desktop::state::TabsState;
use desktop::ui::{TabEntry, Tabs};

/// A session tab entry with this id.
fn session(id: &str) -> TabEntry {
    TabEntry::Session(SessionId::from(id.to_string()))
}

/// Tabs holding `entries`, the one at `active` selected.
fn tabs(entries: &[TabEntry], active: Option<usize>) -> Tabs {
    let mut tabs = Tabs::default();
    for entry in entries {
        tabs.open(entry.clone());
    }
    if let Some(active) = active {
        tabs.activate(active);
    }
    tabs
}

#[test]
fn open_reuses_an_existing_entry() {
    let mut tabs = tabs(&[session("a"), session("b")], Some(1));

    let index = tabs.open(session("a"));

    assert_eq!(index, 0);
    assert_eq!(tabs.entries().len(), 2);
}

#[test]
fn close_activates_the_right_neighbor() {
    let mut tabs = tabs(&[session("a"), session("b"), session("c")], Some(1));

    let closed = tabs.close(1);

    assert_eq!(closed, Some(session("b")));
    assert_eq!(tabs.entries(), &[session("a"), session("c")]);
    assert_eq!(tabs.active_entry(), Some(&session("c")));
}

#[test]
fn closing_the_last_tab_activates_the_left_one() {
    let mut tabs = tabs(&[session("a"), session("b")], Some(1));

    tabs.close(1);

    assert_eq!(tabs.active(), Some(0));
    tabs.close(0);
    assert_eq!(tabs.active(), None);
}

#[test]
fn move_keeps_the_same_entry_active() {
    let mut tabs = tabs(&[session("a"), session("b"), session("c")], Some(0));

    assert!(tabs.move_tab(0, 2));

    assert_eq!(tabs.entries(), &[session("b"), session("c"), session("a")]);
    assert_eq!(tabs.active_entry(), Some(&session("a")));
    assert!(!tabs.move_tab(0, 9));
}

#[test]
fn replace_rekeys_a_draft_in_place() {
    let draft = TabEntry::Draft {
        workspace: Some(PathBuf::from("/w")),
    };
    let mut tabs = tabs(&[session("a"), draft.clone()], Some(1));

    assert!(tabs.replace(&draft, session("new")));

    assert_eq!(tabs.entries(), &[session("a"), session("new")]);
    assert_eq!(tabs.active_entry(), Some(&session("new")));
    assert!(!tabs.replace(&draft, session("other")));
}

#[test]
fn persisted_lists_only_session_tabs() {
    let draft = TabEntry::Draft { workspace: None };
    let tabs = tabs(
        &[draft, session("a"), TabEntry::Settings, session("b")],
        Some(3),
    );

    let persisted: TabsState = tabs.persisted();

    assert_eq!(persisted.open, ["a".to_string(), "b".to_string()]);
    assert_eq!(persisted.active, Some(1));
}

#[test]
fn retain_keeps_the_active_entry_when_it_survives() {
    let mut tabs = tabs(&[session("a"), session("b"), session("c")], Some(2));

    tabs.retain(|tab| *tab != session("a"));

    assert_eq!(tabs.active_entry(), Some(&session("c")));
    tabs.retain(|tab| *tab == session("b"));
    assert_eq!(tabs.active_entry(), Some(&session("b")));
}
