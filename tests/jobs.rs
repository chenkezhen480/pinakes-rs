//! Job status outlives a restart (design, "The librarian"): saved to the tenant's store as jobs
//! are queued and finish; a finished job answers as it did, one the restart cut off answers
//! failed with what to do, and deleting a user takes their jobs with them.

use crate::common::app::{Caller, TestApp};
use axum::http::StatusCode;
use pinakes::jobs::{INTERRUPTED, JobResult, JobState};
use pinakes::tenants::{tenant_dir_name, user_dir_name};

#[tokio::test]
async fn job_status_outlives_a_restart() {
    let mut app = TestApp::new();
    let key = app.create_tenant("acme").await;
    let dir = tenant_dir_name("acme");
    let lily = user_dir_name("lily");
    let page = format!("users/{lily}/memories/likes-tea");
    let result = JobResult {
        pages_created: vec![page.clone()],
        ..Default::default()
    };
    let done = {
        let result = result.clone();
        app.state
            .jobs
            .run(
                &dir,
                Some(&lily),
                "memory",
                Box::pin(async move { Ok(result) }),
            )
            .await
    };
    // A job that never finishes before the service stops, and one queued behind it.
    let cut = app
        .state
        .jobs
        .submit(&dir, None, "expertise", Box::pin(std::future::pending()));
    let waiting = app.state.jobs.submit(
        &dir,
        None,
        "approve",
        Box::pin(async { Ok(JobResult::default()) }),
    );
    app.state.jobs.flush().await;
    app.restart();

    let c = Caller::new(&key);
    let lily_c = c.user(Some("lily"));
    let (status, job) = app.get(lily_c, &format!("/jobs/{}", done.job)).await;
    assert_eq!(status, StatusCode::OK, "{job}");
    assert_eq!(job["status"], "done");
    assert_eq!(job["result"]["pages_created"], serde_json::json!([page]));
    // Still only its owner's.
    let (status, _) = app
        .get(c.user(Some("ana")), &format!("/jobs/{}", done.job))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    for id in [&cut.job, &waiting.job] {
        let (status, job) = app.get(c, &format!("/jobs/{id}")).await;
        assert_eq!(status, StatusCode::OK, "{job}");
        assert_eq!(job["status"], "failed", "{job}");
        assert_eq!(job["error"], INTERRUPTED);
        assert!(job["finished_at"].is_string());
    }
    // A new job runs on the tenant's queue as before.
    let next = app
        .state
        .jobs
        .run(
            &dir,
            None,
            "test",
            Box::pin(async { Ok(JobResult::default()) }),
        )
        .await;
    assert_eq!(next.status, JobState::Done, "{:?}", next.error);

    // Deleting Lily removes her job, saved copy included.
    let (status, body) = app.delete(c, "/users/lily").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = app.get(lily_c, &format!("/jobs/{}", done.job)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    app.state.jobs.flush().await;
    app.restart();
    let (status, _) = app.get(lily_c, &format!("/jobs/{}", done.job)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Finished jobs expire from the store as from memory.
#[tokio::test]
async fn expired_jobs_are_not_brought_back() {
    let mut app = TestApp::with_config("[jobs]\nkeep_finished_minutes = 0\n");
    let key = app.create_tenant("acme").await;
    let dir = tenant_dir_name("acme");
    let done = app
        .state
        .jobs
        .run(
            &dir,
            None,
            "test",
            Box::pin(async { Ok(JobResult::default()) }),
        )
        .await;
    app.state.jobs.flush().await;
    app.restart();
    let (status, _) = app
        .get(Caller::new(&key), &format!("/jobs/{}", done.job))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
