//! Per-request context (e.g. URI for RFC 9457 `instance`).

tokio::task_local! {
    pub static REQUEST_PATH: String;
}
