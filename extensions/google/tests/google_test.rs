use serde_json::json;
use serial_test::serial;
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use google_ext::{client, tools};

fn set_env(oauth: &str, gmail: &str) {
    std::env::set_var("GOOGLE_OAUTH_TOKEN_URL", oauth);
    std::env::set_var("GOOGLE_GMAIL_URL", gmail);
    std::env::set_var("GOOGLE_CLIENT_ID", "cid");
    std::env::set_var("GOOGLE_CLIENT_SECRET", "csecret");
    std::env::set_var("GOOGLE_REFRESH_TOKEN", "rtoken");
    std::env::set_var("GOOGLE_HTTP_TIMEOUT_SECS", "3");
    client::reset_state();
}

async fn dispatch(name: &'static str, args: serde_json::Value) -> Result<serde_json::Value, tools::ToolError> {
    tokio::task::spawn_blocking(move || tools::dispatch(name, &args))
        .await
        .expect("join")
}

async fn mount_token_ok(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("refresh_token=rtoken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "ya29.fake-access",
            "expires_in": 3600,
            "scope": "https://www.googleapis.com/auth/gmail.readonly",
            "token_type": "Bearer"
        })))
        .mount(server)
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn status_reports_credentials_present() {
    let server = MockServer::start().await;
    let token = format!("{}/token", server.uri());
    let gmail = format!("{}/gmail/v1", server.uri());
    set_env(&token, &gmail);

    let out = dispatch("status", json!({})).await.expect("ok");
    assert_eq!(out["ok"], true);
    assert_eq!(out["credentials_present"], true);
    assert!(out["endpoints"]["token"].as_str().unwrap().contains("/token"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn gmail_list_refreshes_token_then_returns_ids() {
    let server = MockServer::start().await;
    let token_url = format!("{}/token", server.uri());
    let gmail_url = format!("{}/gmail/v1", server.uri());
    set_env(&token_url, &gmail_url);
    mount_token_ok(&server).await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .and(header("Authorization", "Bearer ya29.fake-access"))
        .and(query_param("maxResults", "5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messages": [
                {"id":"m1","threadId":"t1"},
                {"id":"m2","threadId":"t2"}
            ],
            "nextPageToken": "page-2",
            "resultSizeEstimate": 42
        })))
        .mount(&server)
        .await;

    let out = dispatch("gmail_list", json!({"max_results": 5}))
        .await.expect("ok");
    assert_eq!(out["count"], 2);
    assert_eq!(out["messages"][0]["id"], "m1");
    assert_eq!(out["next_page_token"], "page-2");
    assert_eq!(out["estimate_total"], 42);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn gmail_list_propagates_query_param() {
    let server = MockServer::start().await;
    let token_url = format!("{}/token", server.uri());
    let gmail_url = format!("{}/gmail/v1", server.uri());
    set_env(&token_url, &gmail_url);
    mount_token_ok(&server).await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .and(query_param("q", "is:unread"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messages": [{"id":"u1","threadId":"u1"}]
        })))
        .mount(&server)
        .await;

    let out = dispatch(
        "gmail_list",
        json!({"query":"is:unread","max_results":10}),
    )
    .await
    .expect("ok");
    assert_eq!(out["count"], 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn token_refresh_failure_surfaces_unauthorized() {
    let server = MockServer::start().await;
    let token_url = format!("{}/token", server.uri());
    let gmail_url = format!("{}/gmail/v1", server.uri());
    set_env(&token_url, &gmail_url);

    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "Token has been expired or revoked."
        })))
        .mount(&server)
        .await;

    let err = dispatch("gmail_list", json!({})).await.unwrap_err();
    assert_eq!(err.code, -32011);
    assert!(err.message.contains("invalid_grant") || err.message.contains("refresh"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn gmail_401_surfaces_unauthorized() {
    let server = MockServer::start().await;
    let token_url = format!("{}/token", server.uri());
    let gmail_url = format!("{}/gmail/v1", server.uri());
    set_env(&token_url, &gmail_url);
    mount_token_ok(&server).await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .respond_with(ResponseTemplate::new(401).set_body_string("auth expired"))
        .mount(&server)
        .await;

    let err = dispatch("gmail_list", json!({})).await.unwrap_err();
    assert_eq!(err.code, -32011);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn gmail_429_surfaces_rate_limited_with_retry_after() {
    let server = MockServer::start().await;
    let token_url = format!("{}/token", server.uri());
    let gmail_url = format!("{}/gmail/v1", server.uri());
    set_env(&token_url, &gmail_url);
    mount_token_ok(&server).await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "15")
                .set_body_string("slow down"),
        )
        .mount(&server)
        .await;

    let err = dispatch("gmail_list", json!({})).await.unwrap_err();
    assert_eq!(err.code, -32013);
    assert!(err.message.contains("15"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn max_results_out_of_range_rejected_locally() {
    let server = MockServer::start().await;
    let token_url = format!("{}/token", server.uri());
    let gmail_url = format!("{}/gmail/v1", server.uri());
    set_env(&token_url, &gmail_url);
    // No wiremock routes — if the validator passes we'd get a transport error,
    // but it must short-circuit before even asking for a token.
    let err = dispatch("gmail_list", json!({"max_results": 999999}))
        .await.unwrap_err();
    assert_eq!(err.code, -32602);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn missing_credentials_surfaces_unauthorized() {
    std::env::remove_var("GOOGLE_REFRESH_TOKEN");
    let server = MockServer::start().await;
    let token_url = format!("{}/token", server.uri());
    let gmail_url = format!("{}/gmail/v1", server.uri());
    // Set others but not refresh_token.
    std::env::set_var("GOOGLE_OAUTH_TOKEN_URL", &token_url);
    std::env::set_var("GOOGLE_GMAIL_URL", &gmail_url);
    std::env::set_var("GOOGLE_CLIENT_ID", "cid");
    std::env::set_var("GOOGLE_CLIENT_SECRET", "csecret");
    client::reset_state();

    let err = dispatch("gmail_list", json!({})).await.unwrap_err();
    assert_eq!(err.code, -32011);
    assert!(err.message.contains("REFRESH_TOKEN") || err.message.contains("refresh_token"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn unknown_tool_is_method_not_found() {
    let server = MockServer::start().await;
    let token_url = format!("{}/token", server.uri());
    let gmail_url = format!("{}/gmail/v1", server.uri());
    set_env(&token_url, &gmail_url);
    let err = dispatch("nope", json!({})).await.unwrap_err();
    assert_eq!(err.code, -32601);
}

// ---- calendar / tasks / drive smoke tests ---------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn calendar_list_events_maps_items() {
    let server = MockServer::start().await;
    let token_url = format!("{}/token", server.uri());
    let gmail_url = format!("{}/gmail/v1", server.uri());
    set_env(&token_url, &gmail_url);
    std::env::set_var(
        "GOOGLE_CALENDAR_URL",
        format!("{}/calendar/v3", server.uri()),
    );
    mount_token_ok(&server).await;

    Mock::given(method("GET"))
        .and(path("/calendar/v3/calendars/primary/events"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                {
                    "id":"e1","summary":"Reunión",
                    "start":{"dateTime":"2026-04-24T15:00:00-05:00"},
                    "end":{"dateTime":"2026-04-24T16:00:00-05:00"},
                    "status":"confirmed"
                }
            ]
        })))
        .mount(&server).await;

    let out = dispatch(
        "calendar_list_events",
        json!({"max_results": 10}),
    ).await.expect("ok");
    assert_eq!(out["count"], 1);
    assert_eq!(out["events"][0]["summary"], "Reunión");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn calendar_create_requires_write_flag() {
    let server = MockServer::start().await;
    let token_url = format!("{}/token", server.uri());
    let gmail_url = format!("{}/gmail/v1", server.uri());
    set_env(&token_url, &gmail_url);
    std::env::remove_var("GOOGLE_ALLOW_CALENDAR_WRITE");

    let err = dispatch("calendar_create_event", json!({
        "summary":"x","start":"2026-04-24","end":"2026-04-24"
    })).await.unwrap_err();
    assert_eq!(err.code, -32043);
    assert!(err.message.contains("GOOGLE_ALLOW_CALENDAR_WRITE"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn tasks_list_lists_returns_summary() {
    let server = MockServer::start().await;
    let token_url = format!("{}/token", server.uri());
    let gmail_url = format!("{}/gmail/v1", server.uri());
    set_env(&token_url, &gmail_url);
    std::env::set_var("GOOGLE_TASKS_URL", format!("{}/tasks/v1", server.uri()));
    mount_token_ok(&server).await;

    Mock::given(method("GET"))
        .and(path("/tasks/v1/users/@me/lists"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id":"@default","title":"My Tasks","updated":"2026-04-20T10:00:00Z"}]
        })))
        .mount(&server).await;

    let out = dispatch("tasks_list_lists", json!({})).await.expect("ok");
    assert_eq!(out["count"], 1);
    assert_eq!(out["lists"][0]["id"], "@default");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn tasks_add_requires_write_flag() {
    let server = MockServer::start().await;
    let token_url = format!("{}/token", server.uri());
    let gmail_url = format!("{}/gmail/v1", server.uri());
    set_env(&token_url, &gmail_url);
    std::env::remove_var("GOOGLE_ALLOW_TASKS_WRITE");

    let err = dispatch("tasks_add", json!({
        "list_id":"@default","title":"buy milk"
    })).await.unwrap_err();
    assert_eq!(err.code, -32043);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn drive_list_maps_files() {
    let server = MockServer::start().await;
    let token_url = format!("{}/token", server.uri());
    let gmail_url = format!("{}/gmail/v1", server.uri());
    set_env(&token_url, &gmail_url);
    std::env::set_var("GOOGLE_DRIVE_URL", format!("{}/drive/v3", server.uri()));
    mount_token_ok(&server).await;

    Mock::given(method("GET"))
        .and(path("/drive/v3/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "files": [
                {"id":"f1","name":"notas.md","mimeType":"text/markdown","size":"123"}
            ],
            "nextPageToken": null
        })))
        .mount(&server).await;

    let out = dispatch("drive_list", json!({"page_size":10})).await.expect("ok");
    assert_eq!(out["files"][0]["id"], "f1");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn drive_upload_requires_write_flag() {
    let server = MockServer::start().await;
    let token_url = format!("{}/token", server.uri());
    let gmail_url = format!("{}/gmail/v1", server.uri());
    set_env(&token_url, &gmail_url);
    std::env::remove_var("GOOGLE_ALLOW_DRIVE_WRITE");

    // Actual upload rejection happens before sandbox check — env flag first.
    let err = dispatch("drive_upload", json!({
        "source_path":"/tmp/whatever.bin"
    })).await.unwrap_err();
    assert_eq!(err.code, -32043);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn drive_download_enforces_sandbox() {
    let server = MockServer::start().await;
    let token_url = format!("{}/token", server.uri());
    let gmail_url = format!("{}/gmail/v1", server.uri());
    set_env(&token_url, &gmail_url);
    // Constrict the sandbox.
    let sb = tempfile::tempdir().unwrap().into_path();
    std::env::set_var("GOOGLE_DRIVE_SANDBOX_ROOT", &sb);

    let err = dispatch("drive_download", json!({
        "id":"f1","output_path":"/etc/evil.bin"
    })).await.unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("outside sandbox"));
}

// ---- people / photos smoke tests ---------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn contacts_search_returns_flattened_people() {
    let server = MockServer::start().await;
    let token_url = format!("{}/token", server.uri());
    let gmail_url = format!("{}/gmail/v1", server.uri());
    set_env(&token_url, &gmail_url);
    std::env::set_var("GOOGLE_PEOPLE_URL", format!("{}/v1", server.uri()));
    mount_token_ok(&server).await;

    Mock::given(method("GET"))
        .and(path("/v1/people:searchContacts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                { "person": {
                    "resourceName":"people/c111",
                    "names":[{"givenName":"Jorge","familyName":"Rodríguez","displayName":"Jorge Rodríguez"}],
                    "emailAddresses":[{"value":"jorge.r@empresax.com","type":"work"}],
                    "organizations":[{"name":"EmpresaX","title":"Director"}]
                }}
            ]
        })))
        .mount(&server).await;

    let out = dispatch("contacts_search", json!({"query":"Jorge"})).await.expect("ok");
    assert_eq!(out["count"], 1);
    let p = &out["results"][0];
    assert_eq!(p["resource_name"], "people/c111");
    assert_eq!(p["display_name"], "Jorge Rodríguez");
    assert_eq!(p["emails"][0]["value"], "jorge.r@empresax.com");
    assert_eq!(p["organization"], "EmpresaX");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn contacts_create_requires_write_flag() {
    let server = MockServer::start().await;
    let token_url = format!("{}/token", server.uri());
    let gmail_url = format!("{}/gmail/v1", server.uri());
    set_env(&token_url, &gmail_url);
    std::env::remove_var("GOOGLE_ALLOW_CONTACTS_WRITE");

    let err = dispatch("contacts_create", json!({
        "given_name":"Jorge",
        "emails":[{"label":"work","value":"x@y.com"}]
    })).await.unwrap_err();
    assert_eq!(err.code, -32043);
    assert!(err.message.contains("GOOGLE_ALLOW_CONTACTS_WRITE"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn contacts_get_bad_resource_name_rejected() {
    let server = MockServer::start().await;
    let token_url = format!("{}/token", server.uri());
    let gmail_url = format!("{}/gmail/v1", server.uri());
    set_env(&token_url, &gmail_url);
    let err = dispatch("contacts_get", json!({"resource_name":"not-a-valid-name"}))
        .await.unwrap_err();
    assert_eq!(err.code, -32602);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn photos_search_rejects_album_plus_filters() {
    let server = MockServer::start().await;
    let token_url = format!("{}/token", server.uri());
    let gmail_url = format!("{}/gmail/v1", server.uri());
    set_env(&token_url, &gmail_url);

    let err = dispatch(
        "photos_search",
        json!({
            "album_id":"al1",
            "date_from":"2025-01-01",
            "date_to":"2025-01-31"
        }),
    ).await.unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("album_id"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn photos_search_filters_serialize_correctly() {
    let server = MockServer::start().await;
    let token_url = format!("{}/token", server.uri());
    let gmail_url = format!("{}/gmail/v1", server.uri());
    set_env(&token_url, &gmail_url);
    std::env::set_var("GOOGLE_PHOTOS_URL", format!("{}/v1", server.uri()));
    mount_token_ok(&server).await;

    Mock::given(method("POST"))
        .and(path("/v1/mediaItems:search"))
        .and(body_string_contains("dateFilter"))
        .and(body_string_contains("LANDSCAPES"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "mediaItems":[
                {"id":"m1","filename":"beach.jpg","baseUrl":"https://lh3/abc","mimeType":"image/jpeg"}
            ]
        })))
        .mount(&server).await;

    let out = dispatch(
        "photos_search",
        json!({
            "date_from":"2026-04-01",
            "date_to":"2026-04-23",
            "content_categories":["LANDSCAPES"],
            "page_size": 5
        }),
    ).await.expect("ok");
    assert_eq!(out["count"], 1);
    assert_eq!(out["media"][0]["filename"], "beach.jpg");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn photos_list_albums_maps_fields() {
    let server = MockServer::start().await;
    let token_url = format!("{}/token", server.uri());
    let gmail_url = format!("{}/gmail/v1", server.uri());
    set_env(&token_url, &gmail_url);
    std::env::set_var("GOOGLE_PHOTOS_URL", format!("{}/v1", server.uri()));
    mount_token_ok(&server).await;

    Mock::given(method("GET"))
        .and(path("/v1/albums"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "albums": [
                {"id":"a1","title":"Vacaciones 2025","mediaItemsCount":"42","coverPhotoBaseUrl":"https://lh3/x"}
            ]
        })))
        .mount(&server).await;

    let out = dispatch("photos_list_albums", json!({})).await.expect("ok");
    assert_eq!(out["count"], 1);
    assert_eq!(out["albums"][0]["title"], "Vacaciones 2025");
}
