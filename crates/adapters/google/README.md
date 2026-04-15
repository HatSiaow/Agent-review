# adapter-google

Purpose: Google Business Profile integration. Fetches raw reviews, maps them into `domain::Review`, refreshes OAuth tokens, and posts owner replies within platform limits.

Key entry points: `GoogleReviewClient`, `HttpGoogleClient`, `InMemoryGoogleClient`, `GoogleConfig`, `normalize_google_review`, `dedup_key`, `star_rating_to_u8`. Internal helpers live in `src/normalize.rs`.

Why tests matter: Wrong field mapping or star enums corrupt review state, dedup keys, and downstream drafting. Bad auth/retry behavior can skip reviews or spam failing posts. Integration tests against mocked HTTP catch URL and pagination regressions before they hit production accounts.

Local tests:

```
cargo test -p adapter-google
```
