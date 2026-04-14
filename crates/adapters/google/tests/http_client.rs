use adapter_google::{GoogleConfig, GoogleReviewClient, HttpGoogleClient};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg_for(base: &str, token_url: &str) -> GoogleConfig {
    GoogleConfig {
        account_id: "acc".into(),
        location_id: "loc".into(),
        poll_interval_secs: 600,
        api_base_url: base.into(),
        oauth_token_url: token_url.into(),
        oauth_client_id: "cid".into(),
        oauth_client_secret: "csec".into(),
        oauth_refresh_token: "rtok".into(),
    }
}

#[tokio::test]
async fn list_reviews_paginates_until_no_next_page_token() {
    let api = MockServer::start().await;
    let token = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "abc",
            "expires_in": 3600
        })))
        .mount(&token)
        .await;

    Mock::given(method("GET"))
        .and(path("/v4/accounts/acc/locations/loc/reviews"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "reviews": [{"reviewId":"r1","starRating":"FIVE","reviewer":{"displayName":"A"},"createTime":"2026-04-10T00:00:00Z"}],
            "nextPageToken":"t2"
        })))
        .mount(&api)
        .await;

    // Second page — match same path; token is in query string which we ignore for simplicity.
    Mock::given(method("GET"))
        .and(path("/v4/accounts/acc/locations/loc/reviews"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "reviews": [{"reviewId":"r2","starRating":"FOUR","reviewer":{"displayName":"B"},"createTime":"2026-04-10T00:00:00Z"}]
        })))
        .mount(&api)
        .await;

    let cfg = cfg_for(&api.uri(), &format!("{}/token", token.uri()));
    let client = HttpGoogleClient::new();
    let res = tokio::task::spawn_blocking(move || client.list_reviews(&cfg))
        .await
        .unwrap()
        .unwrap();

    assert!(res.len() >= 2);
}

