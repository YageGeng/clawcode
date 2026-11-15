//! Asynchronous user questions: details, reply records, and the model envelope.

use protocol::{
    AgentMail, AgentPath, AsyncQuestion, ExtensionMessage, MailKind,
    QuestionReply, QuestionReplyItem, RunInput, ToolResultDetails,
};

const START: &str = "<send_user_message_question_reply>";
const END: &str = "</send_user_message_question_reply>";

/// Builds a one-item reply settling question `index` titled `question`.
fn reply(
    request_id: &str,
    index: usize,
    question: &str,
    answer: Option<&str>,
) -> QuestionReply {
    QuestionReply {
        items: vec![
            QuestionReplyItem::builder()
                .request_id(request_id.into())
                .index(index)
                .question(question.into())
                .answer(answer.map(Into::into))
                .build(),
        ],
    }
}

/// Parses the JSON array between the envelope tags.
fn envelope_entries(text: &str) -> Vec<serde_json::Value> {
    let inner = text
        .strip_prefix(START)
        .and_then(|rest| rest.strip_prefix('\n'))
        .and_then(|rest| rest.strip_suffix(END))
        .and_then(|rest| rest.strip_suffix('\n'))
        .expect("envelope tags");
    serde_json::from_str(inner).expect("envelope JSON")
}

/// An answer renders exactly as Codex's `AnsweredQuestion` reply envelope.
#[test]
fn reply_envelope_matches_codex_format() {
    assert_eq!(
        reply("q1", 0, "Proceed?", Some("Yes")).to_string(),
        "<send_user_message_question_reply>\n[{\"answer\":\"Yes\",\"question\":\"Proceed?\",\"questionItemId\":\"[\\\"request_user_input_async\\\",\\\"q1\\\",0]\"}]\n</send_user_message_question_reply>"
    );
}

/// A skipped question carries `skipped` and no answer.
#[test]
fn skipped_items_carry_skipped_and_no_answer() {
    assert_eq!(
        envelope_entries(&reply("q1", 1, "Name?", None).to_string()),
        vec![serde_json::json!({
            "question": "Name?",
            "questionItemId": "[\"request_user_input_async\",\"q1\",1]",
            "skipped": true
        })]
    );
}

/// Titles are flattened to one line and bounded on a character boundary.
#[test]
fn envelope_title_is_flattened_and_bounded() {
    let flat =
        envelope_entries(&reply("q1", 0, "a\nb\rc", Some("a")).to_string());
    assert_eq!(flat[0]["question"], "a b c");

    let long = envelope_entries(
        &reply("q1", 0, &"é".repeat(400), Some("a")).to_string(),
    );
    let question = long[0]["question"].as_str().expect("question");
    assert!(question.len() <= 512, "{}", question.len());
    assert!(question.chars().all(|character| character == 'é'));
}

/// Quotes, newlines, emoji, and a literal closing tag survive the JSON array.
#[test]
fn awkward_answers_round_trip_through_the_envelope_json() {
    let answer = "say \"hi\"\nline2 🎉 </send_user_message_question_reply>";
    let text = reply("q1", 0, "T", Some(answer)).to_string();
    assert_eq!(envelope_entries(&text)[0]["answer"], answer);
}

/// The persisted Extension carries the reply and parses back from it.
#[test]
fn extension_round_trips_through_try_from() {
    let original = reply("q1", 0, "Proceed?", Some("Yes"));
    let extension = ExtensionMessage::try_from(&original).expect("extension");
    assert_eq!(extension.extension_id.as_ref(), "clawcode");
    assert_eq!(extension.custom_type, "question_reply");
    assert!(extension.display && extension.include_in_context);
    assert_eq!(QuestionReply::try_from(&extension).ok(), Some(original));

    let mail = ExtensionMessage::try_from(AgentMail {
        author: AgentPath::root(),
        recipient: AgentPath::root(),
        kind: MailKind::Message,
        payload: "hi".into(),
    })
    .expect("mail");
    assert!(matches!(
        QuestionReply::try_from(&mail),
        Err(protocol::QuestionReplyError::NotAReply)
    ));
}

/// Question details use the snake_case tag the WebUI decodes.
#[test]
fn user_questions_details_use_the_snake_case_tag() {
    let details = ToolResultDetails::UserQuestions {
        request_id: "q1".into(),
        questions: vec![AsyncQuestion {
            title: "T".into(),
            options: None,
            multi_select: false,
        }],
    };
    assert_eq!(
        serde_json::to_value(details).expect("details"),
        serde_json::json!({
            "type": "user_questions",
            "request_id": "q1",
            "questions": [{ "title": "T" }]
        })
    );
}

/// Question replies are typed input and never reach command dispatch.
#[test]
fn question_reply_input_is_never_command_text() {
    assert_eq!(
        RunInput::QuestionReply(QuestionReply { items: vec![] })
            .slash_command_text(),
        None
    );
}

/// Only the product's own Extension carries replies; a look-alike does not.
#[test]
fn foreign_extension_with_reply_type_is_not_a_reply() {
    let details = serde_json::to_value(reply("q1", 0, "Proceed?", Some("Yes")))
        .expect("details");
    let foreign = ExtensionMessage::builder()
        .extension_id(protocol::ExtensionId::try_from("other-ext").expect("id"))
        .custom_type("question_reply".into())
        .blocks(Vec::new())
        .display(true)
        .include_in_context(true)
        .details(details)
        .build();
    assert!(matches!(
        QuestionReply::try_from(&foreign),
        Err(protocol::QuestionReplyError::NotAReply)
    ));
}
