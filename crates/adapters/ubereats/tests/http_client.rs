use adapter_ubereats::{HttpUberEatsClient, UberEatsConfig, UberEatsReviewClient};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
    let client = HttpUberEatsClient::new();
    let res = tokio::task::spawn_blocking(move || client.list_reviews(&cfg))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(res.len(), 1);
}

