use serde_json::json;
use serial_test::serial;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use github::{client, tools};

fn set_endpoint(server: &MockServer) {
    std::env::set_var("GITHUB_API_URL", server.uri());
    std::env::set_var("GITHUB_TOKEN", "test-token-xxxx");
    std::env::set_var("GITHUB_HTTP_TIMEOUT_SECS", "2");
    std::env::remove_var("GITHUB_DEFAULT_REPO");
    client::reset_state();
}

async fn dispatch(name: &'static str, args: serde_json::Value) -> Result<serde_json::Value, tools::ToolError> {
    tokio::task::spawn_blocking(move || tools::dispatch(name, &args))
        .await
        .expect("join")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_status_ok_with_user() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .and(header("Authorization", "Bearer test-token-xxxx"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "login": "octocat",
            "id": 583231,
            "name": "The Octocat"
        })))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let res = dispatch("status", json!({})).await.expect("ok");
    assert_eq!(res["ok"], true);
    assert_eq!(res["token_present"], true);
    assert_eq!(res["auth"]["login"], "octocat");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_pr_list_ok() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/lordmacu/agent-rs/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "number": 42,
                "title": "feat: add weather extension",
                "state": "open",
                "draft": false,
                "user": {"login": "alice"},
                "head": {"sha": "abc123"},
                "base": {"ref": "main"},
                "html_url": "https://github.com/lordmacu/agent-rs/pull/42",
                "created_at": "2026-04-22T10:00:00Z",
                "updated_at": "2026-04-23T14:00:00Z"
            }
        ])))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let res = dispatch("pr_list", json!({"repo": "lordmacu/agent-rs"})).await.expect("ok");
    assert_eq!(res["count"], 1);
    assert_eq!(res["pulls"][0]["number"], 42);
    assert_eq!(res["pulls"][0]["head_sha"], "abc123");
    assert_eq!(res["pulls"][0]["user"], "alice");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_pr_view_ok() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "number": 7,
            "title": "fix: bug",
            "state": "open",
            "head": {"sha": "deadbeef"},
        })))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let res = dispatch("pr_view", json!({"repo": "o/r", "number": 7})).await.expect("ok");
    assert_eq!(res["number"], 7);
    assert_eq!(res["head"]["sha"], "deadbeef");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_pr_checks_two_step() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "number": 3,
            "head": {"sha": "f00ba4"}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/commits/f00ba4/check-runs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total_count": 1,
            "check_runs": [{"name": "ci", "status": "completed", "conclusion": "success"}]
        })))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let res = dispatch("pr_checks", json!({"repo": "o/r", "number": 3})).await.expect("ok");
    assert_eq!(res["head_sha"], "f00ba4");
    assert_eq!(res["checks"]["check_runs"][0]["conclusion"], "success");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_issue_list_filters_pulls() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"number": 10, "title": "real issue", "state": "open", "user": {"login": "u"}, "labels": [], "html_url": "x", "comments": 0, "created_at": "t", "updated_at": "t"},
            {"number": 11, "title": "this is a PR", "pull_request": {"url": "..."}, "state": "open", "user": {"login":"u"}}
        ])))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let res = dispatch("issue_list", json!({"repo": "o/r"})).await.expect("ok");
    assert_eq!(res["count"], 1, "PRs should be filtered out");
    assert_eq!(res["issues"][0]["number"], 10);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_unauthorized_maps_to_error_code() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"message": "Bad credentials"})))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let err = dispatch("pr_list", json!({"repo": "o/r"})).await.unwrap_err();
    assert_eq!(err.code, -32011);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_rate_limit_detection() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-reset", "1799999999")
                .set_body_json(json!({"message": "API rate limit exceeded"})),
        )
        .mount(&server)
        .await;
    set_endpoint(&server);

    let err = dispatch("pr_list", json!({"repo": "o/r"})).await.unwrap_err();
    assert_eq!(err.code, -32013);
    assert!(err.message.contains("rate"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_5xx_retried_then_fails() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let err = dispatch("pr_list", json!({"repo": "o/r"})).await.unwrap_err();
    assert_eq!(err.code, -32003);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_repo_validation_rejects_bad_format() {
    let server = MockServer::start().await;
    set_endpoint(&server);

    let err = dispatch("pr_list", json!({"repo": "no-slash"})).await.unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("owner/repo"));
}
