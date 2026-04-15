# poster

Purpose: Publishes approved reply drafts back to Google or Uber Eats. Maps adapter errors, enforces draft state via `DraftFsm`, and retries transient failures with backoff.

Key entry points: `PlatformPoster`, `HttpPlatformPoster`, `InMemoryPoster`, `validate_for_posting`, `post_with_retries`, `PosterConfig` in `src/lib.rs`.

Why tests matter: Posting non-approved text is a severe safety issue. Retry semantics must not loop on permanent platform errors. Tests keep posting eligibility aligned with domain draft states before text reaches public review threads.

Local tests:

```
cargo test -p poster
```
