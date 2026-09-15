# Shorten: Rust on Lambda

An incognito rule-based link shortener: anyone posts a URL, gets a ten-character code and a management secret
shown once, and the redirect picks a target from the viewer's country, region, platform and device. Clicks are
counted from CloudFront's access logs by a nightly Athena rollup, never by a write in the request path. Layout,
commands and deploy steps are in `README.md`. The data model, the per-Lambda flows and every deliberate ceiling
with its named upgrade path are in `specs/01-dynamodb-data-model.md`; read it before changing behaviour, and
record a new ceiling there rather than in code. The AWS resources live in the shared `aws-cloud` repository,
whose `README.md` holds the always-free-tier budgets and the $5-a-month ceiling every change must respect; this
repository deploys code only. This file is the rules and the reasons behind them.

## 1. Coding standards

- **No comments of any kind.** Not inline, not block, not `///`, not `//!`, in Rust, JavaScript, shell or
  Python. A comment drifts away from the code beside it and nothing fails when it does; rationale written here
  or in the spec is read on purpose and reviewed with the decision it explains. Express intent through naming
  and structure, and if a name needs a comment, rename it or extract a named helper until it does not.
- Dependencies are workspace-managed in the root `Cargo.toml`; member crates take `{ workspace = true }`. One
  version of a crate across the workspace means one copy in every binary and one upgrade when a advisory lands.
  Check `Cargo.lock` before adding anything: most needs are already present transitively. **No `anyhow`** — it
  erases the type of a failure, and every failure here has to answer an HTTP status.
- `AppError` in `crates/shared/src/error.rs` is the one fallible-API type: a `thiserror` enum whose variants are
  alphabetical, each mapping to a status through `status_code()` and a wire code through `code()`, with a
  `public_message()` that refuses to leak an internal detail and `From` impls that absorb the DynamoDB, builder
  and `serde_dynamo` errors so every `?` works without a `map_err`. Lambda mains convert it into
  `lambda_runtime::Error` at the runtime boundary and nowhere else, because that is the only place the process
  stops caring which failure it was.
- Guards return early with the specific variant: `BadRequest` for input, `NotFound` for an absent row,
  `Unauthorized` for a wrong secret, `RateLimited` for a throttled creator. Use
  `let … else` for an absent row so the happy path stays at one indent.
- **A type lives once**, in the module that owns it. A second crate imports it and never redeclares a copy: two
  declarations of the same shape drift, and the drift shows up as a serialisation bug in production rather than
  a compile error in CI.
- **Every partition key value goes through `table::partition` and is never built by hand.** The `shorten#`
  prefix is what lets this table be shared with `donation`, and it is exactly what the platform's IAM
  `dynamodb:LeadingKeys` condition matches. A key assembled inline with a typo does not read the wrong rows, it
  fails closed with an access-denied error — which is the right outcome and an expensive one to debug.
- The DynamoDB primitives in `table.rs` — `get`, `put`, `update`, `delete`, `batch_get`, `item`, `key`, `s`,
  `n` — are `pub(crate)`. `table.rs` owns only the handle, the key helpers and those primitives; every access to
  a row is a named `DynamoRepo` method in the feature module that owns the row, so the set of things this app
  does to the table is enumerable by reading three files rather than by grepping for `put_item`.
- Each Lambda crate's `[[bin]]` is named after the package, never `bootstrap`. `cargo lambda build` renames the
  artifact to `bootstrap` inside `target/lambda/<package>/` on its own, and `cargo lambda watch` serves it at
  `/lambda-url/<package>/`; a binary actually called `bootstrap` collides across the workspace and is
  unreachable locally.
- Each Lambda is `src/lib.rs`, holding the handler, plus a thin `src/main.rs` that is the runtime boundary and
  nothing else. The handler is then callable from a test without a runtime.
- Alphabetical ordering wherever there is a list with no other meaningful order: enum variants, match arms,
  struct fields, imports, function arguments in a builder. A fixed order makes a diff show what changed rather
  than where something was inserted.
- Reach for the language before a helper: `let … else`, let chains, `LazyLock` for a process-wide value,
  `is_none_or` over a hand-rolled `map_or(true, …)`, `as_deref` over `Option<&Vec<_>>`, `Option::filter` over
  an `if` that returns `None`, `try_for_each` over a `for` that only propagates `?`, `zip(1..)` over
  `enumerate` plus one, `RangeInclusive::contains` over two comparisons, `vec![x; n]` over a mapped range.
- **Enumerate, never repeat.** N things handled the same way are one `enum` with an `ALL` array and one
  exhaustive `match` per behaviour, iterated with `all` or `try_for_each` — `link::Dimension` is the shape.
  Handling written N times means a new case can be forgotten in one of the N places and nothing says so;
  with an enum, the case that is not wired into every `match` is a compile error.
- **No parallel collections.** Strings that must agree — a placeholder in an expression and the name it
  binds, an assignment and its value key — are declared together in one `const` of one struct, never pushed
  in lockstep to three `Vec`s. `link::Field` is the shape; lockstep pushes are how `:url` and `:ur` diverge.
- **A DynamoDB expression or condition is a named `const`**, and the name says what it guards:
  `IF_PRESENT`, `ROLLUP_ONCE_PER_DAY`. Comments are banned, so a `const` name is the only place in code the
  reason for a condition can live; a string literal inside a call has no name and therefore no reason.
- **A rule the spec states gets a predicate named for it.** "A read that sees an expired row treats it as
  absent" is `Link::is_live`, not `link.ttl < now.timestamp()` inline: the spec sentence and the code then
  share a name, and a search for either finds the other.
- `expect` is allowed on operating-system entropy only, because a kernel that cannot produce random bytes is
  not a condition this process can recover from.

## 2. Async and concurrency

- Handlers do their work inline and return. **Never `tokio::spawn` fire-and-forget inside a Lambda**: the
  sandbox freezes the instant the response is returned, so the spawned task dies silently and the work it was
  going to do simply never happens.
- Nothing here should block the runtime. SHA-256 and OS entropy are microseconds. If real CPU work ever
  arrives, hand it to `tokio::task::spawn_blocking` and await it.
- **Every loop over the network is bounded.** The create loop by its eight code attempts, the stats read by its
  eight batch rounds, the rollup by the number of rows Athena returned. An unbounded retry against a throttled
  shared table is how one application takes the other one down.
- **Retries sleep a jittered interval, never a fixed one.** `table::backoff` doubles a 50 ms step to a 3 s
  ceiling and multiplies by a random fraction. A fixed sleep re-synchronises everything that lost the same
  round, so the retry arrives as the same burst that caused the throttle.
- A collision on a code is not a transient failure and is not backed off. The replacement code is drawn
  independently of the one that collided, so sleeping first changes nothing but latency.
- **The rollup paces its writes.** It writes up to two items per active link in one nightly burst against 15
  write units shared with `donation`. Left ungoverned it trips the platform's write-throttle alarm, which is
  `donation`'s alarm, at two in the morning. Keep the pacing and keep the jittered retry on a throttled write.

## 3. Tracing and logging

- Every main calls `shared::telemetry::init_logging()` first; the output is one JSON object per line with event
  fields flattened to the top level. Log with `tracing::{info,warn,error}!` plus fields, never `println!` or
  `eprintln!` — those bypass the level filter and the JSON layout, so they are unsearchable in Logs Insights.
- **Never log an IP address, a User-Agent, a referer or a target URL.** "No PII" is a property of this product,
  not only of the data model, and a log line is as durable as a row. The IP exists inside one rate-limit
  partition key and nowhere else; the target URL identifies what a particular viewer was sent to. Fields carry
  ids and outcomes: `code`, `segment`, `status`, `outcome`, `error`, `day`.
- Instrument new public async functions with `#[tracing::instrument(skip_all, fields(...))]`, naming ids only.
  `skip_all` is the default because an argument logged by accident is how a URL ends up in CloudWatch.
- A handler that can answer 5xx logs it at error level with a `status` field, so a metric filter can find it
  without parsing a message.
- Metrics are EMF lines through `shared::telemetry::{emit, count}`. **The budget is two custom metrics**, since
  the platform's ten free ones are spent by `donation`. Prefer zero: a metric earns its place only by being the
  thing an alarm watches. A dimension multiplies the series, so none are dimensioned.

## 4. Error handling

- DynamoDB errors convert through `From` into `AppError::Dynamo`. Never stringify one early: `status_code()`,
  `public_message()` and `is_condition_failed()` all need the typed error, and a string is a one-way door.
- A conditional write returns `Result<bool, AppError>` through `table::condition_failed_as_false`. `false`
  means the condition was lost and is never an error: a code that was already taken, a link that was already
  deleted, a day that was already rolled up are all ordinary outcomes with their own answers.
- `is_condition_failed` demands an actual `ConditionalCheckFailed` reason before it calls a cancelled
  transaction a lost condition. Capacity is fixed at the free 25 units, so a throttled write is a live failure
  mode here, and reading a throttle as "someone else got there first" would make the rollup silently skip a
  day's numbers.
- Never swallow a `Result` on the table path with `.ok()`, `.unwrap_or_default()` or `let _ =`. An absent
  request header defaulting to empty is fine, because the check that follows then rejects it.
- The origin-verify check runs before anything else on both HTTP functions, and is skipped only when
  `ORIGIN_VERIFY` is unset, which is what makes local development work. Do not add a second escape hatch.
- The redirect path never answers an error page with detail. An unknown code, an invalid code and an expired
  link are all the same minimal 404, because distinguishing them tells an enumerator which guesses were close.
- **A `BadRequest` message is user-facing copy.** The page renders a refusal verbatim, so the server owns the
  wording: it names a rule by the position the page shows, counting from 1, names the dimension, and reads as
  a sentence — `rule 2: devices contains "watch"`. A message that numbers from 0 is not an internal detail;
  it is the sentence a person reads under the form.

## 5. Testing and linting

- Pure rules get unit tests beside them in a `#[cfg(test)] mod tests`, **table-driven with a `cases` array and
  a label per case**. A label turns a failure from "assertion failed at line 214" into a sentence naming the
  behaviour that broke.
- Anything touching the table gets an integration test in the crate's `tests/` against DynamoDB Local through
  `shared::testing`. `local_repo` prints the skip notice itself when `AWS_ENDPOINT_URL_DYNAMODB` is unset, so a
  test body opens with `let Some(repo) = local_repo("x-test").await else { return; };` and nothing else. Its
  table name carries fresh entropy rather than a counter: a `-inMemory` DynamoDB Local outlives a run, and a
  deterministic name fails the second one with `ResourceInUseException`.
- A test asserts one behaviour and its name says which one. A name joined with "and" over two arrangements is
  two tests.
- Every case-table row, and every assertion whose expectation is not self-evident, carries a message.
  **Never assert a compound boolean**: `assert!(a && !b)` says nothing about which half broke, and comparing a
  tuple says exactly which. Assert the error variant, not `is_err`. State cardinality once with a slice pattern
  in a `let … else` rather than asserting a length and then subscripting.
- The mirror of that rule for the happy path: `if let Err(refused) = validate(&rules) { panic!("{refused}") }`,
  never `assert!(v.is_ok(), "{:?}", v.err())`, which computes the value twice and asserts a boolean.
- **Every public message is asserted verbatim by at least one test.** A variant-only assertion checks that
  something was refused and nothing about what the person reads; `rule 0:` shipped under
  `matches!(refused, Err(AppError::BadRequest(_)))` for exactly that reason.
- **A ceiling is tested on both sides**: the last value accepted and the first refused, for every limit —
  twenty rules and twenty-one, thirty values and thirty-one, a three-character region and a four. A limit
  tested on one side is a limit whose other side is a guess.
- Expect the value the system produced, not a constant restating it. Name fixtures for their role, never for
  their values.
- An invariant of an endpoint belongs in the helper every test fetches through, not in the one test that
  happened to notice it. A repeated `get`/`unwrap` chain becomes a helper named for the question it answers.
- Build rows through `shared::testing` — `link`, `rule`, `stats_day` — rather than hand-rolling a struct
  literal a helper already produces, so a new field on `Link` breaks one function instead of thirty.
- **A bug fix lands with the test that would have caught it, in the same change.** A fix without one is a
  promise that the bug was understood, with nothing holding the promise.
- The edge function and the page's modules are tested with `node --test "edge/**/*.test.mjs"
  "frontend/**/*.test.mjs"`. Quote the globs: `node --test edge/` on Node 22 tries to require the directory
  and fails. The page's own conventions are in `frontend/CLAUDE.md`, the same rules `donation/frontend`
  follows; this file stays Rust and infrastructure.
- The page the redirect function serves is built by `lambdas/redirect/build.rs` from `frontend/`, inside
  `cargo build`, into `OUT_DIR`. Never commit a generated page and never add a Node build step: the deploy
  image has no Node, and the bundler is two hundred lines of `std` whose refusals are table-tested.
- `rust-toolchain.toml` pins the toolchain and declares `clippy` and `rustfmt`, so the dev container, CI and any
  rustup environment resolve the same compiler. Bump the pin deliberately, in its own change, with the suite
  green.
- **No cargo on the host.** In the dev container run `cargo fmt --all`, `cargo clippy --workspace
  --all-targets` and `cargo test --workspace`. `.github/workflows/test.yml` runs all three against a DynamoDB
  Local service container on every pull request and push to `main`, with the endpoint set, so the integration
  tests run rather than skip — a green suite that skipped everything is worse than no suite, because it is
  believed.
- `rustfmt.toml` sets the line width. Format before submitting. Address clippy warnings; never `#[allow]`
  without the reason written into the spec.

## 6. Performance and memory

- Every DynamoDB round trip is latency and a slice of 25 shared provisioned units. **A redirect is exactly one
  eventually consistent `GetItem` and nothing else** — no counter increment, no index lookup, no second read.
  That is the whole reason the product fits in the free tier, and it is the first thing any change must
  preserve.
- An invalid code is rejected on length and character set before any network call, so walking a dictionary of
  paths costs an invocation and no capacity.
- No scans, ever; the runtime role does not grant `Scan`. No N+1 gets: a stats window is one chunked
  `BatchGetItem`, not a loop of hundred point reads.
- Hoist invariants out of loops and use a `HashSet` or `HashMap` for membership rather than `contains` inside a
  loop.
- Borrow rather than clone. `DynamoRepo` is cheap to clone because it wraps a client; domain structs are not.
  Clone only to hand ownership to a row write.
- The functions are arm64 with `lto` and a single codegen unit. Keep dependency features minimal so cold starts
  stay short: the redirect path's p95 is a user waiting on a link, and the whole stack has to fit 400k
  GB-seconds a month.
- **The cache key is the path plus the single query string `s`.** Adding a header or a cookie to it multiplies
  the key space by every value that header takes and stops the redirect cache hitting, which turns a free cache
  hit into a Lambda invocation and a table read. `SEGMENT_INCLUDE_REGION` in `edge/segment.js` is the one lever
  for shrinking the key space, and flipping it stops region rules matching.

## 7. Infrastructure

- **A change to an alarm, a schedule, a function URL, an IAM policy, a cache policy or an environment variable
  is a pull request to `aws-cloud`, never to this repository.** There is no `infra/` directory here and no CDK
  anywhere; `apps/shorten.tf` is the whole surface. Keeping deployment of code and deployment of resources
  apart is what lets this repository's CI be three cargo commands with no credentials.
- **Three CloudWatch alarms, and no fourth.** `shorten-redirect-errors`, `shorten-mgmt-errors` and
  `shorten-rollup-failed`. The platform's ten free alarms are entirely spent by `donation` and the table's own
  throttle alarms, so each of these bills about ten cents a month and is most of what this app costs. No
  canary, for the same reason: the redirect errors alarm already watches the path that is the product.
- `edge/segment.js` is deployed by this repository's workflow over a placeholder that `aws-cloud` commits with
  `lifecycle { ignore_changes = [code] }`, because Terraform cannot read across repositories. The placeholder
  emits `s=XX%7CXX%7Cother%7Cother`, the same value the real function emits for a viewer it cannot classify,
  so a distribution running it answers correctly from the default URL instead of erroring.
- **The segment separator is percent-encoded on the wire, in the edge function and in the placeholder.** A
  Lambda function URL answers a query string holding a raw `|` with a 400 before the function is invoked,
  and CloudFront forwards the value exactly as the viewer-request function wrote it, so a raw separator
  turns every redirect into a 400 that no Lambda log ever sees. The redirect and the rollup both decode
  `%7C` before they split, and the local proxy accepts either spelling, which is why only a deployed stack
  shows the difference.
- The three functions receive `TABLE_NAME`, `METRIC_NAMESPACE`, `ORIGIN_VERIFY`, `ATHENA_OUTPUT`,
  `ATHENA_WORKGROUP`, `GLUE_DATABASE`, `GLUE_TABLE` and `LOGS_BUCKET`. Adding a variable is a change in
  `aws-cloud` first; reading one that was never set is a cold-start panic.
