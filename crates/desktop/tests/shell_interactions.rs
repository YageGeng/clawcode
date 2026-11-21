//! Docked interactions: questions, the plan and todo panel, and waiting sessions.

mod support;

use agent_client_protocol::v2::schema as wire;
use desktop::backend::SessionId;
use desktop::session::SessionStatus;
use desktop::state::DesktopState;
use desktop::ui::QuestionAnswer;
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{App, SharedString, TestAppContext};
use support::shell::{Live, click_label, eventually, live, painted, shows};

/// Sends one prompt without waiting for the run to settle.
fn say(live: &Live, id: &SessionId, text: &str) {
    live.backend
        .block_on(live.backend.client.prompt(
            id,
            vec![wire::ContentBlock::Text(wire::TextContent::new(text))],
        ))
        .expect("prompt");
}

/// Number of questions still open in the session.
fn open_questions(live: &Live, id: &SessionId, cx: &App) -> usize {
    live.session(id, cx)
        .map(|handle| handle.read(cx).projection().open_questions().len())
        .unwrap_or_default()
}

#[test]
fn answers_join_picks_in_order_with_free_text_last() {
    let options = ["a".to_string(), "b".to_string(), "c".to_string()];

    let multi = QuestionAnswer::builder()
        .options(&options)
        .picks(&[2, 0])
        .other(Some("more"))
        .multi_select(true)
        .build();
    let single = QuestionAnswer::builder()
        .options(&options)
        .picks(&[1])
        .multi_select(false)
        .build();
    let blank = QuestionAnswer::builder()
        .options(&options)
        .picks(&[])
        .other(Some("  "))
        .multi_select(false)
        .build();

    assert_eq!(multi.text(), "- a\n- c\n- more");
    assert_eq!(single.text(), "b");
    assert_eq!(blank.text(), "");
}

#[gpui_kit::test]
fn question_panel_answers_with_the_chosen_option(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    say(&live, &id, "ask");
    live.until(cx, "question panel", |window| shows(window, "Pick one"));

    live.window(cx, |window, cx| click_label(window, "b", cx));
    live.window(cx, |window, cx| {
        window.click(SharedString::from("question-send"), cx)
    });

    eventually(cx, "first question settled", |cx| {
        open_questions(&live, &id, cx) == 1
    });
    live.until(cx, "next question", |window| {
        shows(window, "Anything else?")
    });
}

#[gpui_kit::test]
fn skip_dismisses_the_question(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    say(&live, &id, "ask");
    live.until(cx, "question panel", |window| shows(window, "Pick one"));

    live.window(cx, |window, cx| {
        window.click(SharedString::from("question-skip"), cx)
    });

    eventually(cx, "question skipped", |cx| {
        open_questions(&live, &id, cx) == 1
    });
}

#[gpui_kit::test]
fn skipped_question_text_does_not_carry_over(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    say(&live, &id, "ask");
    live.until(cx, "question panel", |window| shows(window, "Pick one"));

    // Choosing "Other" puts the cursor in its input.
    live.window(cx, |window, cx| click_label(window, "Other", cx));
    live.window(cx, |window, _| {
        assert_eq!(window.find("question-send").disabled(), Some(true))
    });
    live.window(cx, |window, cx| window.input("use postgres", cx));
    live.until(cx, "typed answer", |window| {
        window.find("question-send").disabled() != Some(true)
    });
    live.window(cx, |window, cx| {
        window.click(SharedString::from("question-skip"), cx)
    });

    // The next question starts empty: one click cannot send the skipped text.
    live.until(cx, "next question", |window| {
        shows(window, "Anything else?")
    });
    live.window(cx, |window, _| {
        assert_eq!(window.find("question-send").disabled(), Some(true));
    });

    // A question without options is answered with typed text alone.
    live.window(cx, |window, cx| window.input("nothing", cx));
    live.until(cx, "free-text answer", |window| {
        window.find("question-send").disabled() != Some(true)
    });
    live.window(cx, |window, cx| {
        window.click(SharedString::from("question-send"), cx)
    });
    eventually(cx, "all questions settled", |cx| {
        open_questions(&live, &id, cx) == 0
    });
}

#[gpui_kit::test]
fn stop_withdrawing_a_queued_answer_returns_the_question_quietly(
    cx: &mut TestAppContext,
) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    // The run that asks keeps working, so the answer waits in the queue behind it.
    say(&live, &id, "askwork");
    live.until(cx, "question panel", |window| shows(window, "Pick one"));
    live.window(cx, |window, cx| {
        window.click(SharedString::from("question-skip"), cx)
    });
    live.until(cx, "free-text question", |window| {
        shows(window, "Anything else?")
    });

    live.until(cx, "the asking run still working", |window| {
        window.try_find("composer-stop").is_some()
    });
    live.until(cx, "question during the run", |window| {
        window.try_find("question-input").is_some()
    });
    live.window(cx, |window, cx| {
        window.click(SharedString::from("question-input"), cx)
    });
    live.window(cx, |window, cx| window.input("later", cx));
    live.until(cx, "answer typed", |window| {
        window.find("question-send").disabled() != Some(true)
    });
    live.window(cx, |window, cx| {
        window.click(SharedString::from("question-send"), cx)
    });
    eventually(cx, "answer queued", |cx| {
        open_questions(&live, &id, cx) == 0
    });

    // Stop withdraws the queued answer: the user's own choice, not a failure.
    live.window(cx, |window, cx| {
        window.click(SharedString::from("composer-stop"), cx)
    });
    eventually(cx, "question back", |cx| {
        open_questions(&live, &id, cx) == 1
    });
    live.window(cx, |window, cx| {
        window.render_frame(cx);
        assert!(window.notifications(cx).is_empty());
    });
}

#[gpui_kit::test]
fn background_question_marks_the_session_waiting(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let asking = live.open_new_session(cx);
    let other = live.open_new_session(cx);

    say(&live, &asking, "ask");

    eventually(cx, "waiting status", |cx| {
        live.view.read(cx).session_status(&asking, cx) == SessionStatus::Waiting
    });
    live.window(cx, |window, cx| {
        assert!(!window.has_active_dialog(cx));
        assert!(!shows(window, "Pick one"));
    });
    live.view
        .update(cx, |view, cx| view.open_session(&asking, cx));
    live.until(cx, "docked question", |window| shows(window, "Pick one"));
    live.view
        .update(cx, |view, cx| view.open_session(&other, cx));
}

#[gpui_kit::test]
fn plan_panel_shows_todo_progress(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);

    live.run(cx, &id, vec!["todo".into()]);

    live.until(cx, "plan panel header", |window| {
        shows(window, "Plan") && shows(window, "0/2")
    });
    // The run is over, so the panel is collapsed: its items are hidden.
    live.window(cx, |window, _| assert!(!shows(window, "Write code")));
    live.window(cx, |window, cx| {
        window.click(SharedString::from("plan-panel-toggle"), cx)
    });
    live.until(cx, "todo items", |window| {
        painted(window).iter().any(|text| {
            text.text() == "Writing code" || text.text() == "Write code"
        })
    });
}

/// Puts the session into plan mode and asks for a plan.
fn ask_for_plan(live: &Live, id: &SessionId) {
    live.backend
        .block_on(live.backend.client.set_config_option(id, "mode", "plan"))
        .expect("plan mode");
    say(live, id, "plan");
}

/// Number of pending agent requests of the session.
fn requests(live: &Live, id: &SessionId, cx: &App) -> usize {
    live.session(id, cx)
        .map(|handle| handle.read(cx).projection().controls().requests().len())
        .unwrap_or_default()
}

/// Whether the session's `plan-1` tool call has completed.
fn plan_tool_completed(live: &Live, id: &SessionId, cx: &App) -> bool {
    live.session(id, cx).is_some_and(|handle| {
        let projection = handle.read(cx).projection();
        (0..projection.len()).any(|index| {
            matches!(projection.row(index),
                Some(desktop::projection::TranscriptRow::Tool(tool))
                    if tool.tool_call_id.to_string() == "plan-1"
                        && tool.status == desktop::projection::ToolStatus::Completed)
        })
    })
}

#[gpui_kit::test]
fn plan_approval_card_approves(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    ask_for_plan(&live, &id);
    live.until(cx, "approval card", |window| {
        shows(window, "Execute this plan?") && shows(window, "step one")
    });

    live.window(cx, |window, cx| {
        window.click(SharedString::from("request-approve"), cx)
    });

    eventually(cx, "plan executed", |cx| {
        requests(&live, &id, cx) == 0 && plan_tool_completed(&live, &id, cx)
    });
    live.window(cx, |window, _| {
        assert!(!shows(window, "Execute this plan?"))
    });
}

#[gpui_kit::test]
fn keep_planning_keeps_the_run_planning(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    ask_for_plan(&live, &id);
    live.until(cx, "approval card", |window| {
        shows(window, "Execute this plan?")
    });

    live.window(cx, |window, cx| {
        window.click(SharedString::from("request-reject"), cx)
    });

    eventually(cx, "card answered and run settled", |cx| {
        requests(&live, &id, cx) == 0
            && live.view.read(cx).session_status(&id, cx) == SessionStatus::Idle
    });
}

#[gpui_kit::test]
fn long_approved_plan_scrolls_inside_the_panel(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    live.backend
        .block_on(live.backend.client.set_config_option(&id, "mode", "plan"))
        .expect("plan mode");
    say(&live, &id, "longplan");
    live.until(cx, "approval card", |window| {
        shows(window, "Execute this plan?")
    });
    live.window(cx, |window, cx| {
        window.click(SharedString::from("request-approve"), cx)
    });
    eventually(cx, "plan approved and run settled", |cx| {
        requests(&live, &id, cx) == 0
            && live.view.read(cx).session_status(&id, cx) == SessionStatus::Idle
    });
    // The settled run folds the panel; open it by hand.
    live.until(cx, "plan panel", |window| shows(window, "Plan"));
    if live.window(cx, |window, _| window.try_find("plan-panel-body").is_none())
    {
        live.window(cx, |window, cx| {
            window.click(SharedString::from("plan-panel-toggle"), cx)
        });
    }
    live.until(cx, "approved plan", |window| shows(window, "Approved plan"));

    // Sixty steps are far taller than 20 rem: the body scrolls instead of
    // pushing the transcript and the composer out of the card.
    live.window(cx, |window, _| {
        let body = window.try_find("plan-panel-body").expect("panel body");
        let limit = f32::from(window.rem_size()) * 20.;
        assert!(
            f32::from(body.bounds().size.height) <= limit + 1.,
            "plan panel body is {:?} tall, more than 20 rem",
            body.bounds().size.height
        );
        let viewport = window.viewport_size();
        let composer = window
            .try_find("composer-send")
            .expect("send button")
            .bounds();
        assert!(
            composer.bottom() <= viewport.height,
            "the composer stays inside the window"
        );
    });
}

/// Focuses the active composer.
fn focus_composer(live: &Live, cx: &mut TestAppContext) {
    live.window(cx, |window, cx| {
        let composer = live
            .view
            .read(cx)
            .active_view()
            .expect("active view")
            .read(cx)
            .composer()
            .clone();
        composer.update(cx, |composer, cx| composer.focus(window, cx));
    });
}

#[gpui_kit::test]
fn withdrawn_approval_card_disappears(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    ask_for_plan(&live, &id);
    live.until(cx, "approval card", |window| {
        shows(window, "Execute this plan?")
    });

    focus_composer(&live, cx);
    live.window(cx, |window, cx| window.press("escape", cx));

    eventually(cx, "request withdrawn", |cx| requests(&live, &id, cx) == 0);
    live.until(cx, "card gone", |window| {
        !shows(window, "Execute this plan?")
    });
}

/// Docks a hand-built form elicitation on the session.
fn dock_elicitation(live: &Live, cx: &mut TestAppContext, id: &SessionId) {
    let request: wire::CreateElicitationRequest =
        serde_json::from_value(serde_json::json!({
            "mode": "form", "sessionId": id.to_string(), "message": "Who are you?",
            "requestedSchema": { "type": "object",
                "properties": { "name": { "type": "string", "title": "Name" } },
                "required": ["name"] },
            "_meta": { "clawcode": { "mcpServerId": "demo" } }
        }))
        .expect("elicitation");
    let handle = cx.update(|cx| live.session(id, cx)).expect("attached");
    handle.update(cx, |handle, cx| {
        handle.controls_mut().push_request(
            desktop::backend::AgentRequest::Elicitation {
                id: desktop::backend::RequestId(9_000),
                request,
            },
        );
        cx.emit(
            desktop::projection::ProjectionChange::builder()
                .controls(true)
                .build(),
        );
        cx.notify();
    });
}

#[gpui_kit::test]
fn elicitation_dialog_submits_values(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    dock_elicitation(&live, cx, &id);
    live.until(cx, "elicitation card", |window| {
        shows(window, "demo needs your input")
    });

    live.window(cx, |window, cx| {
        window.click(SharedString::from("request-fill"), cx)
    });
    live.until(cx, "form dialog", |window| shows(window, "Name *"));
    live.window(cx, |window, cx| {
        window.click(SharedString::from("form-submit"), cx)
    });
    live.until(cx, "required error", |window| shows(window, "Required"));

    // The dialog's first field takes focus on Tab.
    live.window(cx, |window, cx| {
        window.press("tab", cx);
        window.input("x", cx);
    });
    live.window(cx, |window, cx| {
        window.click(SharedString::from("form-submit"), cx)
    });

    eventually(cx, "request answered", |cx| requests(&live, &id, cx) == 0);
    live.window(cx, |window, cx| assert!(!window.has_active_dialog(cx)));
}

#[gpui_kit::test]
fn escape_closes_the_dialog_before_stopping(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let id = live.open_new_session(cx);
    say(&live, &id, "slower");
    eventually(cx, "running", |cx| {
        live.view.read(cx).session_status(&id, cx) == SessionStatus::Running
    });
    dock_elicitation(&live, cx, &id);
    live.until(cx, "elicitation card", |window| {
        shows(window, "demo needs your input")
    });
    live.window(cx, |window, cx| {
        window.click(SharedString::from("request-fill"), cx)
    });
    live.window(cx, |window, cx| assert!(window.has_active_dialog(cx)));

    live.window(cx, |window, cx| window.press("escape", cx));

    live.window(cx, |window, cx| assert!(!window.has_active_dialog(cx)));
    cx.update(|cx| {
        assert_ne!(
            live.view.read(cx).session_status(&id, cx),
            SessionStatus::Idle
        )
    });
}
