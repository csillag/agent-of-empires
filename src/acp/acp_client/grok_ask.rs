//! Grok's `ask_user_question` tool.
//!
//! Grok does not send `elicitation/create`. The shell coordinator calls the
//! client with ACP method `x.ai/ask_user_question` and waits for a tagged
//! `{ "outcome": ... }` result (see grok-build
//! `AskUserQuestionExtRequest` / `AskUserQuestionExtResponse`). AoE already
//! renders that shape of question as an [`Elicitation`] card, so this module
//! turns the Grok payload into that card and turns the card's answer back
//! into the JSON Grok deserializes.
//!
//! Plan-mode extras (chat about this, skip the interview) are not buttons on
//! the card. Skip and Cancel both answer `cancelled`, which Grok treats as
//! "declined", not as a transport error.
//!
//! [`Elicitation`]: crate::acp::elicitations::Elicitation

use std::collections::BTreeMap;

use agent_client_protocol::schema::v1::{
    CreateElicitationResponse, ElicitationAction, ElicitationContentValue,
};
use agent_client_protocol::JsonRpcRequest;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::{mpsc, oneshot};
use tracing::warn;

use crate::acp::approvals::Nonce;
use crate::acp::elicitations::{
    Elicitation, ElicitationFieldKind, ElicitationOption, ElicitationOutcome, ElicitationQuestion,
};
use crate::acp::state::Event;

use super::pending::{
    ElicitationResolutionMessage, PendingResolver, PendingResponder, PendingResponders,
};

const ASK_METHOD: &str = "x.ai/ask_user_question";

/// Wire params of `x.ai/ask_user_question`. camelCase, matching the object
/// Grok's coordinator serializes.
#[derive(Debug, Clone, Serialize, Deserialize, JsonRpcRequest)]
#[request(method = "x.ai/ask_user_question", response = serde_json::Value)]
#[serde(rename_all = "camelCase")]
pub(super) struct GrokAskUserQuestionRequest {
    pub(super) session_id: String,
    tool_call_id: String,
    questions: Vec<GrokQuestion>,
    /// Kept so a plan-mode request still parses. The card has no plan-mode
    /// buttons; see the module comment.
    #[serde(default)]
    #[allow(dead_code)]
    mode: GrokAskMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum GrokAskMode {
    #[default]
    Default,
    Plan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrokQuestion {
    question: String,
    #[serde(default)]
    options: Vec<GrokOption>,
    /// ACP sends `multiSelect`. The model-facing tool arguments use
    /// `multi_select`; accept both so a captured arguments object still parses.
    #[serde(default, alias = "multi_select")]
    multi_select: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GrokOption {
    label: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    preview: Option<String>,
}

fn select_key(index: usize) -> String {
    format!("question_{index}")
}

fn custom_key(index: usize) -> String {
    format!("question_{index}_custom")
}

fn blank_question(field_key: String, kind: ElicitationFieldKind) -> ElicitationQuestion {
    ElicitationQuestion {
        field_key,
        title: None,
        description: None,
        required: false,
        kind,
        options: Vec::new(),
        min_items: None,
        max_items: None,
        min_length: None,
        max_length: None,
        pattern: None,
        format: None,
        minimum: None,
        maximum: None,
        default: None,
    }
}

fn nonempty(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// The question card. One select (or multi-select) plus a free-text "Other"
/// box per question, which is the shape the card already renders for Claude.
/// Neither field is required: an Other-only answer must still submit, and an
/// unanswered question is omitted from Grok's answer map.
pub(super) fn elicitation_from_grok(
    nonce: Nonce,
    request: &GrokAskUserQuestionRequest,
    requested_at: DateTime<Utc>,
) -> Elicitation {
    let mut questions = Vec::with_capacity(request.questions.len() * 2);
    for (index, question) in request.questions.iter().enumerate() {
        let multi = question.multi_select.unwrap_or(false);
        let mut select = blank_question(
            select_key(index),
            if multi {
                ElicitationFieldKind::MultiSelect
            } else {
                ElicitationFieldKind::SingleSelect
            },
        );
        select.title = Some(question.question.clone());
        select.options = question
            .options
            .iter()
            .map(|option| ElicitationOption {
                value: option.label.clone(),
                label: option.label.clone(),
                description: nonempty(&option.description),
            })
            .collect();
        questions.push(select);

        let mut other = blank_question(custom_key(index), ElicitationFieldKind::FreeText);
        other.title = Some("Other".to_string());
        questions.push(other);
    }

    let message = match request.questions.as_slice() {
        [only] => only.question.clone(),
        _ => "The agent has a few questions.".to_string(),
    };

    Elicitation {
        nonce,
        message,
        title: None,
        description: None,
        tool_call_id: Some(request.tool_call_id.clone()),
        questions,
        requested_at,
        resolved: None,
    }
}

fn cancelled() -> Value {
    serde_json::json!({ "outcome": "cancelled" })
}

fn labels_of(content: &BTreeMap<String, ElicitationContentValue>, key: &str) -> Vec<String> {
    match content.get(key) {
        Some(ElicitationContentValue::String(text)) => nonempty(text).into_iter().collect(),
        Some(ElicitationContentValue::StringArray(values)) => {
            values.iter().filter_map(|text| nonempty(text)).collect()
        }
        _ => Vec::new(),
    }
}

/// Grok's accepted/cancelled JSON. Answers are keyed by the question text.
/// A free-text-only answer is the label `"Other"` plus `annotations.notes`.
/// A selected option keeps its labels, and a single-select also echoes that
/// option's `preview` when it had one. Skip, Cancel, and a dropped resolver
/// are all `cancelled`.
pub(super) fn grok_response_from_elicitation(
    request: &GrokAskUserQuestionRequest,
    response: &CreateElicitationResponse,
) -> Value {
    let ElicitationAction::Accept(action) = &response.action else {
        return cancelled();
    };
    let content = action.content.clone().unwrap_or_default();

    let mut answers = Map::new();
    let mut annotations = Map::new();
    for (index, question) in request.questions.iter().enumerate() {
        let mut labels = labels_of(&content, &select_key(index));
        let notes = content
            .get(&custom_key(index))
            .and_then(|value| match value {
                ElicitationContentValue::String(text) => nonempty(text),
                _ => None,
            });
        if labels.is_empty() && notes.is_none() {
            continue;
        }
        if labels.is_empty() {
            labels.push("Other".to_string());
        }

        let multi = question.multi_select.unwrap_or(false);
        let preview = if !multi && labels.len() == 1 {
            question
                .options
                .iter()
                .find(|option| option.label == labels[0])
                .and_then(|option| option.preview.as_deref().and_then(nonempty))
        } else {
            None
        };

        if preview.is_some() || notes.is_some() {
            let mut annotation = Map::new();
            if let Some(preview) = preview {
                annotation.insert("preview".to_string(), Value::String(preview));
            }
            if let Some(notes) = notes.clone() {
                annotation.insert("notes".to_string(), Value::String(notes));
            }
            annotations.insert(question.question.clone(), Value::Object(annotation));
        }
        answers.insert(
            question.question.clone(),
            Value::Array(labels.into_iter().map(Value::String).collect()),
        );
    }

    let mut body = Map::new();
    body.insert("outcome".to_string(), Value::String("accepted".to_string()));
    body.insert("answers".to_string(), Value::Object(answers));
    if !annotations.is_empty() {
        body.insert("annotations".to_string(), Value::Object(annotations));
    }
    Value::Object(body)
}

/// Park a question card and block until the user answers it. The answer
/// travels through the same nonce map as `elicitation/create`, so the web
/// card and `resolve_elicitation` do not grow a second path.
pub(super) async fn handle_grok_ask_request(
    request: GrokAskUserQuestionRequest,
    event_tx: mpsc::Sender<Event>,
    pending: PendingResponders,
) -> Result<Value, agent_client_protocol::Error> {
    if request.questions.is_empty() {
        warn!(
            target: "acp.protocol",
            method = ASK_METHOD,
            "grok ask_user_question had no questions; cancelling"
        );
        return Ok(cancelled());
    }

    let nonce = Nonce::new();
    let elicitation = elicitation_from_grok(nonce.clone(), &request, Utc::now());
    let (resolve_tx, resolve_rx) = oneshot::channel::<ElicitationResolutionMessage>();
    pending.lock().await.insert(
        nonce.clone(),
        PendingResponder {
            resolver: PendingResolver::Elicitation {
                elicitation: Box::new(elicitation.clone()),
                resolver: resolve_tx,
            },
        },
    );

    if event_tx
        .send(Event::ElicitationRequested {
            elicitation: elicitation.clone(),
        })
        .await
        .is_err()
    {
        pending.lock().await.remove(&nonce);
        warn!(
            target: "acp.protocol",
            "grok ask_user_question card could not be shown; cancelling"
        );
        return Ok(cancelled());
    }

    let ElicitationResolutionMessage {
        response,
        outcome,
        answers,
    } = resolve_rx
        .await
        .unwrap_or_else(|_| ElicitationResolutionMessage {
            response: CreateElicitationResponse::new(ElicitationAction::Cancel),
            outcome: ElicitationOutcome::Cancelled,
            answers: Vec::new(),
        });

    let _ = event_tx
        .send(Event::ElicitationResolved {
            nonce,
            outcome,
            answers,
        })
        .await;

    Ok(grok_response_from_elicitation(&request, &response))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::elicitations::{
        build_response, summarize_answers, AnswerValue, ElicitationResolution,
    };
    use agent_client_protocol::JsonRpcMessage;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// Questions copied verbatim from the recipe-dsl Grok session
    /// `01a0db57-53d8-7381-a471-da914492a50d`, chat_history.jsonl tool call
    /// `call-add62236-bde7-4b91-865e-db4b2359716b-30` at 2026-09-26T01:39:08Z.
    /// AoE showed no card. The tool completed 1,800,018 ms later as
    /// "User declined to answer".
    const RECIPE_DSL_ARGUMENTS: &str = r#"{"questions":[{"question":"recipe-dsl 0.13.0 is released: main 0ee5c5b matches origin, signed tag v0.13.0 is on the remote, and the PyPI wheel matches the local file. The 0.13.1 items were left as candidates. Which should I start?","options":[{"label":"Leave 0.13.0 (Recommended)","description":"No code changes. I stay idle until a bug report or until you pick a 0.13.1 item."},{"label":"IF() outside SET","description":"The gap the 0.13.0 changelog already names: IF() in LOG and other non-SET positions still uses the old name rules."},{"label":"Null-page OUTPUT","description":"OUTPUT of the page binding after a null page still emits a null row."},{"label":"DateCheck scalar gap","description":"DateCheck in a scalar context is still not detected by the stream gate."}]}]}"#;

    const RECIPE_DSL_QUESTION: &str = "recipe-dsl 0.13.0 is released: main 0ee5c5b matches origin, signed tag v0.13.0 is on the remote, and the PyPI wheel matches the local file. The 0.13.1 items were left as candidates. Which should I start?";

    fn recipe_dsl_request() -> GrokAskUserQuestionRequest {
        let arguments: Value = serde_json::from_str(RECIPE_DSL_ARGUMENTS).unwrap();
        let wire = serde_json::json!({
            "sessionId": "01a0db57-53d8-7381-a471-da914492a50d",
            "toolCallId": "call-add62236-bde7-4b91-865e-db4b2359716b-30",
            "questions": arguments["questions"],
            "mode": "default",
        });
        serde_json::from_value(wire).unwrap()
    }

    fn request_from(value: Value) -> GrokAskUserQuestionRequest {
        serde_json::from_value(value).unwrap()
    }

    fn content_of(pairs: Vec<(&str, ElicitationContentValue)>) -> CreateElicitationResponse {
        let content: BTreeMap<_, _> = pairs
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect();
        CreateElicitationResponse::new(ElicitationAction::Accept(
            agent_client_protocol::schema::v1::ElicitationAcceptAction::new().content(content),
        ))
    }

    #[test]
    fn method_is_the_grok_ext_method() {
        assert!(GrokAskUserQuestionRequest::matches_method(ASK_METHOD));
        assert!(!GrokAskUserQuestionRequest::matches_method(
            "elicitation/create"
        ));
        assert_eq!(recipe_dsl_request().method(), ASK_METHOD);
    }

    #[test]
    fn recipe_dsl_fixture_becomes_a_single_select_card_with_other() {
        let request = recipe_dsl_request();
        assert_eq!(request.method(), ASK_METHOD);
        let card = elicitation_from_grok(Nonce::new(), &request, Utc::now());

        assert_eq!(
            card.tool_call_id.as_deref(),
            Some("call-add62236-bde7-4b91-865e-db4b2359716b-30")
        );
        assert_eq!(card.message, RECIPE_DSL_QUESTION);
        assert_eq!(card.questions.len(), 2);

        let select = &card.questions[0];
        assert_eq!(select.field_key, "question_0");
        assert_eq!(select.kind, ElicitationFieldKind::SingleSelect);
        assert_eq!(select.title.as_deref(), Some(RECIPE_DSL_QUESTION));
        assert!(!select.required);
        let labels: Vec<_> = select
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                "Leave 0.13.0 (Recommended)",
                "IF() outside SET",
                "Null-page OUTPUT",
                "DateCheck scalar gap",
            ]
        );
        assert_eq!(
            select.options[0].description.as_deref(),
            Some(
                "No code changes. I stay idle until a bug report or until you pick a 0.13.1 item."
            )
        );
        assert_eq!(select.options[0].value, "Leave 0.13.0 (Recommended)");

        let other = &card.questions[1];
        assert_eq!(other.field_key, "question_0_custom");
        assert_eq!(other.kind, ElicitationFieldKind::FreeText);
        assert_eq!(other.title.as_deref(), Some("Other"));
        assert!(!other.required);
    }

    #[test]
    fn recipe_dsl_choice_is_keyed_by_the_question_text() {
        let request = recipe_dsl_request();
        let response = content_of(vec![(
            "question_0",
            ElicitationContentValue::String("Leave 0.13.0 (Recommended)".into()),
        )]);
        let wire = grok_response_from_elicitation(&request, &response);
        assert_eq!(wire["outcome"], "accepted");
        assert!(wire.get("annotations").is_none());
        assert_eq!(
            wire["answers"][RECIPE_DSL_QUESTION],
            serde_json::json!(["Leave 0.13.0 (Recommended)"])
        );
    }

    #[test]
    fn freeform_only_is_other_plus_notes() {
        let request = recipe_dsl_request();
        let response = content_of(vec![(
            "question_0_custom",
            ElicitationContentValue::String("  start the IF() gap  ".into()),
        )]);
        let wire = grok_response_from_elicitation(&request, &response);
        assert_eq!(
            wire["answers"][RECIPE_DSL_QUESTION],
            serde_json::json!(["Other"])
        );
        assert_eq!(
            wire["annotations"][RECIPE_DSL_QUESTION]["notes"],
            "start the IF() gap"
        );
        assert!(wire["annotations"][RECIPE_DSL_QUESTION]
            .get("preview")
            .is_none());
    }

    #[test]
    fn selected_option_keeps_preview_and_typed_notes() {
        let request = request_from(serde_json::json!({
            "sessionId": "s",
            "toolCallId": "t",
            "mode": "default",
            "questions": [{
                "question": "Which layout?",
                "options": [
                    {"label": "Grid", "description": "rows", "preview": "<div>grid</div>"},
                    {"label": "Flex", "description": "line"}
                ]
            }]
        }));
        let response = content_of(vec![
            ("question_0", ElicitationContentValue::String("Grid".into())),
            (
                "question_0_custom",
                ElicitationContentValue::String("use the header row".into()),
            ),
        ]);
        let wire = grok_response_from_elicitation(&request, &response);
        assert_eq!(
            wire["answers"]["Which layout?"],
            serde_json::json!(["Grid"])
        );
        assert_eq!(
            wire["annotations"]["Which layout?"]["preview"],
            "<div>grid</div>"
        );
        assert_eq!(
            wire["annotations"]["Which layout?"]["notes"],
            "use the header row"
        );
    }

    #[test]
    fn multi_select_is_a_list_and_drops_preview() {
        let request = request_from(serde_json::json!({
            "sessionId": "s",
            "toolCallId": "t",
            "questions": [{
                "question": "Which checks?",
                "multiSelect": true,
                "options": [
                    {"label": "Lint", "description": "", "preview": "nope"},
                    {"label": "Tests", "description": "the suite"}
                ]
            }]
        }));
        let card = elicitation_from_grok(Nonce::new(), &request, Utc::now());
        assert_eq!(card.questions[0].kind, ElicitationFieldKind::MultiSelect);
        assert_eq!(card.message, "Which checks?");
        assert!(card.questions[0].options[0].description.is_none());

        let response = content_of(vec![(
            "question_0",
            ElicitationContentValue::StringArray(vec!["Tests".into(), "Lint".into()]),
        )]);
        let wire = grok_response_from_elicitation(&request, &response);
        assert_eq!(
            wire["answers"]["Which checks?"],
            serde_json::json!(["Tests", "Lint"])
        );
        assert!(wire.get("annotations").is_none());
    }

    #[test]
    fn snake_case_multi_select_alias_parses() {
        let request = request_from(serde_json::json!({
            "sessionId": "s",
            "toolCallId": "t",
            "questions": [{
                "question": "Which?",
                "multi_select": true,
                "options": [{"label": "A", "description": "a"}]
            }]
        }));
        assert_eq!(request.questions[0].multi_select, Some(true));
    }

    #[test]
    fn plan_mode_still_parses_as_the_same_card() {
        let mut wire = serde_json::to_value(recipe_dsl_request()).unwrap();
        wire["mode"] = Value::String("plan".into());
        let request: GrokAskUserQuestionRequest = serde_json::from_value(wire).unwrap();
        assert_eq!(request.mode, GrokAskMode::Plan);
        let card = elicitation_from_grok(Nonce::new(), &request, Utc::now());
        assert_eq!(card.questions.len(), 2);
        assert_eq!(card.message, RECIPE_DSL_QUESTION);
    }

    #[test]
    fn skip_and_cancel_are_cancelled() {
        let request = recipe_dsl_request();
        for action in [ElicitationAction::Decline, ElicitationAction::Cancel] {
            let wire =
                grok_response_from_elicitation(&request, &CreateElicitationResponse::new(action));
            assert_eq!(wire, serde_json::json!({"outcome": "cancelled"}));
        }
    }

    #[test]
    fn several_questions_keep_answer_order_and_omit_blanks() {
        let request = request_from(serde_json::json!({
            "sessionId": "s",
            "toolCallId": "t",
            "questions": [
                {"question": "First?", "options": [{"label": "A", "description": "a"}]},
                {"question": "Second?", "options": [{"label": "B", "description": "b"}]},
                {"question": "Third?", "options": [{"label": "C", "description": "c"}]}
            ]
        }));
        let card = elicitation_from_grok(Nonce::new(), &request, Utc::now());
        assert_eq!(card.message, "The agent has a few questions.");
        let keys: Vec<_> = card
            .questions
            .iter()
            .map(|question| question.field_key.as_str())
            .collect();
        assert_eq!(
            keys,
            [
                "question_0",
                "question_0_custom",
                "question_1",
                "question_1_custom",
                "question_2",
                "question_2_custom",
            ]
        );

        let response = content_of(vec![
            ("question_2", ElicitationContentValue::String("C".into())),
            ("question_0", ElicitationContentValue::String("A".into())),
        ]);
        let wire = grok_response_from_elicitation(&request, &response);
        let keys: Vec<_> = wire["answers"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(keys, ["First?", "Third?"]);
    }

    #[tokio::test]
    async fn handler_shows_the_recipe_dsl_card_and_returns_the_picked_label() {
        let (event_tx, mut event_rx) = mpsc::channel(8);
        let pending: PendingResponders = Arc::new(Mutex::new(HashMap::new()));
        let request = recipe_dsl_request();
        let handle = tokio::spawn(handle_grok_ask_request(request, event_tx, pending.clone()));

        let elicitation = loop {
            match event_rx.recv().await.expect("card event") {
                Event::ElicitationRequested { elicitation } => break elicitation,
                other => panic!("unexpected event {other:?}"),
            }
        };
        assert_eq!(
            elicitation.questions[0].options[2].label,
            "Null-page OUTPUT"
        );

        let mut submitted = BTreeMap::new();
        submitted.insert(
            "question_0".to_string(),
            AnswerValue::Text("IF() outside SET".into()),
        );
        let display = summarize_answers(&elicitation, &submitted);
        let outcome = ElicitationOutcome::Accepted;
        let response = build_response(
            &elicitation,
            ElicitationResolution::Accept { answers: submitted },
        )
        .expect("the card's answer is a real option");

        let nonce = elicitation.nonce.clone();
        let PendingResponder { resolver } = pending.lock().await.remove(&nonce).expect("parked");
        let PendingResolver::Elicitation { resolver, .. } = resolver else {
            panic!("elicitation resolver");
        };
        let sent = resolver
            .send(ElicitationResolutionMessage {
                response,
                outcome,
                answers: display,
            })
            .is_ok();
        assert!(sent, "handler still waiting");

        let wire = handle.await.expect("handler").expect("response");
        assert_eq!(wire["outcome"], "accepted");
        assert_eq!(
            wire["answers"][RECIPE_DSL_QUESTION],
            serde_json::json!(["IF() outside SET"])
        );

        match event_rx.recv().await.expect("resolved") {
            Event::ElicitationResolved {
                nonce: got,
                answers,
                ..
            } => {
                assert_eq!(got, nonce);
                assert_eq!(answers[0].question, RECIPE_DSL_QUESTION);
                assert_eq!(answers[0].answer, "IF() outside SET");
            }
            other => panic!("unexpected event {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_question_list_cancels_without_a_card() {
        let (event_tx, mut event_rx) = mpsc::channel(2);
        let pending: PendingResponders = Arc::new(Mutex::new(HashMap::new()));
        let request = request_from(serde_json::json!({
            "sessionId": "s",
            "toolCallId": "t",
            "questions": []
        }));
        let wire = handle_grok_ask_request(request, event_tx, pending)
            .await
            .unwrap();
        assert_eq!(wire, serde_json::json!({"outcome": "cancelled"}));
        assert!(event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn dropped_resolver_cancels_and_clears_the_card() {
        let (event_tx, mut event_rx) = mpsc::channel(4);
        let pending: PendingResponders = Arc::new(Mutex::new(HashMap::new()));
        let request = recipe_dsl_request();
        let handle = tokio::spawn(handle_grok_ask_request(request, event_tx, pending.clone()));
        let nonce = match event_rx.recv().await.expect("card") {
            Event::ElicitationRequested { elicitation } => elicitation.nonce,
            other => panic!("unexpected {other:?}"),
        };
        let PendingResponder { resolver } = pending.lock().await.remove(&nonce).unwrap();
        drop(resolver);

        let wire = handle.await.expect("handler").expect("response");
        assert_eq!(wire, serde_json::json!({"outcome": "cancelled"}));
        match event_rx.recv().await.expect("cleared") {
            Event::ElicitationResolved {
                nonce: got,
                answers,
                outcome,
            } => {
                assert_eq!(got, nonce);
                assert!(answers.is_empty());
                assert_eq!(outcome, ElicitationOutcome::Cancelled);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
