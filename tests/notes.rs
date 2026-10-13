//! Notes (`/ingest/note`, MCP `record_decision_or_lesson`): the agent records a decision, a
//! lesson or both in one call, and the librarian splits it. A decision is filed for the user, a
//! lesson for the team, each as a record of its own citing the note; what the librarian can't
//! split stays the user's. The split is scripted here; what a real model does with the test
//! set's messages is measured by `compare/record_eval.py` and `compare/merge_classify.py`.

use crate::common;

use std::sync::Arc;

use crate::common::app::{Caller, TestApp};
use crate::common::llm::{FakeLlm, OK, Step, all_ok, create, folder, record_of};
use axum::http::StatusCode;
use pinakes::tenants::{group_dir_name, user_dir_name};
use serde_json::{Value, json};

const NOTE: &str = "The parcel was already with the carrier, so the redirect took 5 days; \
                    cancelling and reshipping is faster. We decided to always cancel and reship \
                    once the carrier has the parcel.";

/// A model that splits notes with `split` and files each record as a new page in its folder.
fn model(split: impl Fn(&str) -> Value + Send + Sync + 'static) -> Arc<FakeLlm> {
    let llm = FakeLlm::new(|step, msg, _| match step {
        Step::Plan => {
            let record = record_of(msg);
            let title = record["topic"]
                .as_str()
                .or(record["task"].as_str())
                .unwrap()
                .to_owned();
            let slug = title.to_lowercase().replace(' ', "-");
            json!({ "edits": [create(
                &format!("{}/{slug}", folder(msg)),
                &title,
                "From a note.",
                &["commerce:order"],
                &format!("# {title}\n\n{}", record.to_string().replace('"', "'")),
            )]})
            .to_string()
        }
        Step::Verify => all_ok(msg),
        Step::VerifyDescribe => OK.into(),
        Step::Match => common::llm::NOT_THE_SAME.into(),
        Step::Lint => common::llm::NO_FINDINGS.into(),
        Step::Read => common::llm::skip_to_end(msg),
        Step::Describe => common::llm::describe_from_definition(msg),
        Step::Rank | Step::Screen => unreachable!("not asked here"),
    });
    llm.split_with(split);
    llm
}

fn both(_: &str) -> Value {
    json!({ "items": [
        { "kind": "lesson", "title": "address change after handover",
          "text": "Once the carrier has the parcel, its redirect took 5 days; cancelling and reshipping is faster." },
        { "kind": "decision", "title": "address changes after handover",
          "text": "Always cancel and reship once the carrier has the parcel." },
    ]})
}

async fn shop(llm: &Arc<FakeLlm>) -> (TestApp, String) {
    let app = TestApp::with_llm(llm.llm());
    let key = app.create_tenant("shop").await;
    let (status, body) = app
        .post(
            Caller::new(&key),
            "/ingest/graph/schema",
            common::testset::schema_request("shopline", "commerce"),
        )
        .await;
    assert!(status.is_success(), "{body}");
    (app, key)
}

async fn note(app: &TestApp, caller: Caller<'_>, body: Value) -> Value {
    let (status, resp) = app.post(caller, "/ingest/note", body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{resp}");
    assert_eq!(resp["kind"], "note");
    let job = app.wait_job(caller, resp["job"].as_str().unwrap()).await;
    assert_eq!(job["status"], "done", "{job}");
    job
}

fn sources_in(app: &TestApp) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(app.tenant_dir("shop").join("sources"))
        .map(|d| {
            d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// One note, two records: the lesson for the team, the decision for its user, each citing the
/// note, which is kept as it came. Another user sees the lesson and not the decision.
#[tokio::test]
async fn a_note_is_split_into_a_decision_for_the_user_and_a_lesson_for_the_team() {
    let llm = model(both);
    let (app, key) = shop(&llm).await;
    let lily = Caller::new(&key).user(Some("lily"));
    let job = note(
        &app,
        lily,
        json!({ "topic": "address change after payment", "content": NOTE,
                "context": "Order 1042.", "entities": ["commerce:order"] }),
    )
    .await;
    let r = &job["result"];
    let lily_dir = user_dir_name("lily");
    let split = r["split"].as_array().unwrap();
    assert_eq!(split.len(), 2, "{r}");
    assert_eq!(split[0]["kind"], "experience");
    assert_eq!(split[0]["audience"], "tenant");
    assert_eq!(split[1]["kind"], "decision");
    assert_eq!(split[1]["audience"], lily_dir.as_str());
    let lesson = "experience/address-change-after-handover";
    let decision = format!("users/{lily_dir}/decisions/address-changes-after-handover");
    assert_eq!(r["pages_created"], json!([lesson, decision]), "{r}");

    // The note went to the librarian whole, and each record cites it.
    let note_source = r["source"].as_str().unwrap().trim_start_matches("sources/");
    assert!(note_source.contains("-note-"), "{note_source}");
    assert!(
        llm.notes()[0].contains("redirect took 5 days") && llm.notes()[0].contains("Order 1042.")
    );
    for plan in llm.calls(Step::Plan) {
        assert_eq!(record_of(&plan)["note"], note_source, "{plan}");
    }
    assert_eq!(sources_in(&app).len(), 3, "the note and its two records");

    let wang = Caller::new(&key).user(Some("wang"));
    let (status, _) = app.get(wang, &format!("/pages/{lesson}")).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = app.get(wang, &format!("/pages/{decision}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the decision is lily's");
    let (status, _) = app.get(lily, &format!("/pages/{decision}")).await;
    assert_eq!(status, StatusCode::OK);

    // Deleting lily deletes the note and her decision; the team's lesson stays.
    let (status, body) = app.delete(Caller::new(&key), "/users/lily").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let left = sources_in(&app);
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(left[0].contains("-experience-"), "{left:?}");
    let (status, _) = app.get(wang, &format!("/pages/{lesson}")).await;
    assert_eq!(status, StatusCode::OK);
}

/// A reply the librarian can't use, every time, leaves the whole note as the user's decision:
/// nothing is shown to the team.
#[tokio::test]
async fn a_note_the_librarian_cant_split_stays_the_users() {
    let llm = model(|_| json!({ "items": [] }));
    let (app, key) = shop(&llm).await;
    let lily = Caller::new(&key).user(Some("lily"));
    let job = note(
        &app,
        lily,
        json!({ "topic": "address changes after handover", "content": NOTE }),
    )
    .await;
    let split = job["result"]["split"].as_array().unwrap().clone();
    assert_eq!(split.len(), 1, "{split:?}");
    assert_eq!(split[0]["kind"], "decision");
    assert_eq!(split[0]["audience"], user_dir_name("lily").as_str());
    assert!(llm.notes().len() > 1, "asked again first");
    let plan = llm.calls(Step::Plan).pop().unwrap();
    assert!(
        record_of(&plan)["decision"]
            .as_str()
            .unwrap()
            .contains("5 days")
    );
}

/// A note's parts have different audiences, so only a group fits both; without a user its
/// decisions would have no one to be filed for.
#[tokio::test]
async fn a_note_is_for_its_user_or_one_of_their_groups() {
    let llm = model(both);
    let (app, key) = shop(&llm).await;
    let body = json!({ "topic": "address change", "content": NOTE });
    let (status, resp) = app
        .post(Caller::new(&key), "/ingest/note", body.clone())
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{resp}");
    assert!(resp.to_string().contains("user header"), "{resp}");
    for audience in ["tenant", "user"] {
        let caller = Caller::new(&key).user(Some("lily")).audience(audience);
        let (status, resp) = app.post(caller, "/ingest/note", body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{audience}: {resp}");
    }
    assert!(
        sources_in(&app).is_empty(),
        "nothing kept from a refused note"
    );

    let groups = ["ops".to_owned()];
    let caller = Caller::new(&key)
        .user(Some("lily"))
        .groups(&groups)
        .audience("group:ops");
    let job = note(&app, caller, body).await;
    let ops = group_dir_name("ops");
    for filed in job["result"]["split"].as_array().unwrap() {
        assert_eq!(filed["audience"], ops.as_str(), "{filed}");
    }
    let (status, _) = app
        .get(
            Caller::new(&key).user(Some("wang")),
            "/pages/experience/address-change-after-handover",
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the lesson is the group's");
}
