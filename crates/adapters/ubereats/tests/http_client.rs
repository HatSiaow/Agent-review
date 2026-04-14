use adapter_ubereats::{HttpUberEatsClient, UberEatsConfig, UberEatsReviewClient};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use time::macros::datetime;

fn cfg_for(base: &str, token_url: &str) -> UberEatsConfig {
    UberEatsConfig {
        store_id: "store".into(),
        webhook_secret: "whsec".into(),
        poll_interval_secs: 1800,
        api_base_url: base.into(),
        oauth_token_url: token_url.into(),
        oauth_client_id: "cid".into(),
        oauth_client_secret: "csec".into(),
    }
}

#[tokio::test]
async fn list_reviews_returns_reviews_array() {
    let api = MockServer::start().await;
    let token = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "abc",
            "expires_in": 3600,
            "token_type": "bearer"
        })))
        .mount(&token)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/eats/stores/store/reviews"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "reviews": [{"review_uuid":"r1","rating":{"overall":5}}]
        })))
        .mount(&api)
        .await;

    let cfg = cfg_for(&api.uri(), &format!("{}/token", token.uri()));
    let res = tokio::task::spawn_blocking(move || HttpUberEatsClient::new().list_reviews(&cfg))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(res.len(), 1);
}

#[tokio::test]
async fn list_reviews_since_filters_strictly_by_created_at_but_keeps_unparseable() {
    let api = MockServer::start().await;
    let token = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "abc",
            "expires_in": 3600,
            "token_type": "bearer"
        })))
        .mount(&token)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/eats/stores/store/reviews"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "reviews": [
                { "review_uuid":"old", "rating":{"overall":5}, "created_at":"2026-04-10T00:00:00Z" },
                { "review_uuid":"new", "rating":{"overall":5}, "created_at":"2026-04-10T00:00:01Z" },
                { "review_uuid":"bad", "rating":{"overall":5}, "created_at":"not-a-time" }
            ]
        })))
        .mount(&api)
        .await;

    let cfg = cfg_for(&api.uri(), &format!("{}/token", token.uri()));
    let since = datetime!(2026-04-10 00:00:00 UTC);
    let res = tokio::task::spawn_blocking(move || {
        HttpUberEatsClient::new().list_reviews_since(&cfg, Some(since))
    })
    .await
    .unwrap()
    .unwrap();

    // "old" is excluded (strictly after), "new" included, "bad" kept to avoid missing reviews.
    let ids: Vec<String> = res
        .into_iter()
        .filter_map(|v| v.get("review_uuid").and_then(|s| s.as_str()).map(|s| s.to_string()))
        .collect();
    assert_eq!(ids, vec!["new".to_string(), "bad".to_string()]);
}

