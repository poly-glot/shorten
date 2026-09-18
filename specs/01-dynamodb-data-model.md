# Data model: an incognito rule-based link shortener on DynamoDB

## Overview

This is the storage design for a link shortener with no accounts. Anyone posts a URL and receives a ten-character code and a
management secret shown once; the secret is the only credential that ever exists. A link carries an ordered list of rules, and a
redirect picks its target from the viewer's country, region, platform and device. Clicks are counted from CloudFront's own access
logs, rolled up once a night by Athena and written back into the table, so no click ever writes to DynamoDB in the request path.
The code is Rust on AWS Lambda.

Everything lives in one DynamoDB table, shared with the `donation` app, under the key prefix `shorten#`. The table has two global
secondary indexes; this app uses neither. What a shortener does not need — a user partition, an email index, a session, an audit
trail of who did what — is absent because there is nobody to attribute anything to.

### What the product requires

| Requirement                                                          | Where it lands                                                                                   |
|----------------------------------------------------------------------|--------------------------------------------------------------------------------------------------|
| Anyone can create a link; nobody signs in                            | No entrant, user or session entity exists; the link row is the whole account                     |
| The creator can later edit, delete and read stats                    | `secret_hash` on the link row; the plaintext secret is returned once and never stored            |
| A redirect chooses between targets by country, region, platform, device | `rules`, an ordered list on the link row, evaluated in memory by `shared::link::resolve`        |
| A redirect is one read and nothing else                              | `GetItem` on `shorten#L#{code}`; no counter, no write, no index lookup                           |
| Per-link daily clicks broken down by segment                         | One stats row per link per day, its `seg` map keyed by the canonical segment string              |
| The dashboard never queries Athena                                   | Stats are read by chunked `BatchGetItem` over a date window of at most 100 days                  |
| A creator cannot flood the service                                   | One counter row per IP per day, incremented with `ADD`, refused above 30                         |
| Links expire after five years, stats after ninety days               | The `ttl` attribute on every row this app writes                                                 |
| No PII in the data model                                             | No name, email, User-Agent or referer is ever stored; the IP appears only inside a rate-limit key |

### Design principles

1. **One table, no index.** Every read this app performs is a `GetItem` or a `BatchGetItem` on a key it already knows: the code
   comes from the URL path, the dates come from the requested window, the IP comes from the request. There is nothing to search
   for, so there is nothing to index.
2. **A code is random, not allocated.** A ten-character code from a 64-symbol alphabet is a 60-bit integer drawn from the
   operating system's CSPRNG and inserted with `ConditionExpression: attribute_not_exists(PK)`. A counter would be a single hot
   partition and a global serialisation point; randomness spreads writes across the whole keyspace and costs one conditional put.
3. **The secret is a hash, like a password.** The management secret is 32 CSPRNG bytes in base64url. Only its hex SHA-256 is
   stored, and verification is a constant-time compare. A dump of the table grants nobody the ability to edit a link.
4. **Clicks come from the edge log, never from the request path.** CloudFront logs every request including cache hits, so a
   cached redirect is counted exactly like an origin one. Incrementing a DynamoDB counter per click would reintroduce a hot key
   on a viral link and turn every read into a write.
5. **Athena is a batch engine and stays out of the request path.** One nightly query feeds DynamoDB; every dashboard view is a
   `BatchGetItem`. Query cost then scales with the volume of logs, not with the number of people looking at a dashboard.
6. **The segment travels in the query string.** A viewer-request CloudFront function overwrites the request query string with
   `s={COUNTRY}|{REGION}|{PLATFORM}|{DEVICE}`, the separator percent-encoded as `%7C` because a Lambda function URL refuses a
   raw `|` before the function runs. That one value is simultaneously the cache key and the `cs-uri-query` field of
   every log line, so the analytics pipeline needs no custom log fields and no custom headers.
7. **Rules are data, not code.** A rule is a JSON object stored verbatim in the link row and matched in memory. Editing rules is
   a single `UpdateItem`; there is no deployment, no rule table and no join.
8. **Expiry is the only lifecycle.** Nothing is archived, soft-deleted or swept by a job. Every row carries a `ttl` and DynamoDB
   removes it. A read that sees a row whose `ttl` has passed treats it as absent, so deletion lag is never visible.
9. **No `updatedAt` columns.** The table's change stream records every write and when it happened. Rows carry creation and domain
   timestamps only.

## The table

DynamoDB is a key-value store. Every row has a partition key, which groups rows, and a sort key, which orders rows within a
group. A query reads one partition, optionally a range of sort keys, and cannot join. A global secondary index is a second copy
of the table under different keys, so a row can also be found by another attribute. Several applications share this one table and
are told apart by key prefixes.

The table is the shared `aws-cloud` table. It is provisioned at the free-tier 25 read and 25 write units, split 15 on the table
and 5 on each index, with no autoscaling, and the `donation` app is already using them. It has TTL enabled on the attribute
`ttl`, which is free and affects only rows that carry the attribute. See `modules/table` in `aws-cloud`.

| Attribute | Type   | Purpose                              |
|-----------|--------|--------------------------------------|
| `PK`      | string | partition key                        |
| `SK`      | string | sort key                             |
| `GSI1PK`  | string | partition key of GSI1, unused here   |
| `GSI1SK`  | string | sort key of GSI1, unused here        |
| `GSI2PK`  | string | partition key of GSI2, unused here   |

Every partition key value this app writes begins with `shorten#`. That prefix is what allows the table to be shared, and it is
what the platform's IAM `dynamodb:LeadingKeys` condition matches: the runtime role may touch `shorten#*` and nothing else, so a
key built by hand — with a typo, or with the prefix forgotten — fails closed with an access-denied error rather than reading
another app's rows. The prefix is applied in exactly one function, `table::partition`, and never anywhere else.

Every row this app writes uses the constant sort key `#METADATA`. Sort keys exist to order the rows inside a partition, and each
of these entities is a single row whose identity is entirely in its partition key. Giving stats rows a sort key of the date
instead — one partition per code, one row per day — would allow a range query over a window rather than a batch of point reads,
but it would also put every day of a popular link's history in one partition and make the rollup's nightly burst hammer it. The
batch of point reads spreads across the keyspace and costs the same read units.

## Entity map

| Entity            | PK                                  | SK          | GSI1PK | GSI1SK | GSI2PK |
|-------------------|-------------------------------------|-------------|--------|--------|--------|
| Link              | `shorten#L#{code}`                  | `#METADATA` |        |        |        |
| Daily stats       | `shorten#S#{code}#{YYYY-MM-DD}`     | `#METADATA` |        |        |        |
| Create rate limit | `shorten#R#{ip}#{YYYY-MM-DD}`       | `#METADATA` |        |        |        |

Three entities, three key shapes, no index entries. `code` is ten characters from
`ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_`. Dates are ISO `YYYY-MM-DD` in UTC, so they sort as text in
date order and a stats key for a day is derivable without a read. `ip` is the viewer address as the function received it; it
appears only inside this key and is written to nothing else, not to a row attribute and not to a log line.

## Entities

Attribute names are snake_case on the wire. The Rust structs in `crates/shared/src` are the source of truth. Optional attributes
are absent rather than null.

### Link

| Attribute      | Type   | Example                                   | Notes                                                       |
|----------------|--------|-------------------------------------------|-------------------------------------------------------------|
| `url`          | string | `https://example.com/landing`             | the guaranteed fallback when no rule matches                |
| `rules`        | list   | see below                                 | ordered, at most 20; first match wins                        |
| `secret_hash`  | string | 64 hex characters                         | hex SHA-256 of the management secret                         |
| `created_at`   | number | `1767355200`                              | epoch seconds                                                |
| `ttl`          | number | `1925121600`                              | `created_at` plus 1,826 days, five years                     |
| `clicks_total` | number | `412`                                     | lifetime clicks, advanced by the rollup, never by a redirect |
| `last_rollup`  | string | `2026-01-02`                              | absent until the first rollup; guards the total against double counting |

### Rule

A rule is a JSON object inside `rules`, stored exactly as the API received it.

| Attribute   | Type           | Notes                                                                    |
|-------------|----------------|--------------------------------------------------------------------------|
| `countries` | list of string | ISO 3166-1 alpha-2, uppercase; absent means any country                  |
| `regions`   | list of string | ISO 3166-2 first-level codes, uppercase, one to three characters         |
| `platforms` | list of string | a subset of `android`, `ios`, `other`                                    |
| `devices`   | list of string | a subset of `desktop`, `mobile`, `other`, `tablet`, `tv`                 |
| `url`       | string         | required; validated exactly like the link's own `url`                    |

Each present dimension is a list of accepted values, so values inside one list are ORed. The dimensions present in one rule are
ANDed. An absent dimension is a wildcard. A list holds at most 30 values. A rule with no dimensions at all matches everything and
shadows every rule after it.

```json
{
  "countries": ["IN", "PK"],
  "platforms": ["android"],
  "url": "https://play.google.com/store/apps/details?id=com.example.app"
}
```

That rule sends Android viewers in India and Pakistan to the Play Store and leaves everyone else to the next rule, or to the
link's own `url`.

Target URLs, on the link and on every rule, must be `http` or `https`, at most 2,048 characters, free of whitespace and control
characters, and must name a public domain. IP literals in any notation, `localhost` and its subdomains, `.local`, `.internal`
and the other private suffixes, userinfo hiding a host after an `@`, and trailing-dot evasions are all refused with 422. The
rules live in `shared::url` and the refusals are enumerated in its tests.

### Daily stats

| Attribute | Type   | Example                                    | Notes                                                        |
|-----------|--------|--------------------------------------------|--------------------------------------------------------------|
| `date`    | string | `2026-01-02`                               | the day these clicks fell on, UTC, also part of the key      |
| `clicks`  | number | `47`                                       | the day's total, which is the sum of `seg`                   |
| `seg`     | map    | `{"IN\|MH\|android\|mobile": 31, ...}`     | canonical segment string to count; absent segments are zero  |
| `ttl`     | number | `1775001600`                               | the day plus 90 days                                         |

The row is written whole, never incremented. A rollup for a day replaces the day, which is what makes a replay a no-op rather
than a doubling.

### Create rate limit

| Attribute | Type   | Notes                                                   |
|-----------|--------|----------------------------------------------------------|
| `n`       | number | creates attempted from this IP today, advanced by `ADD` |
| `ttl`     | number | the day plus two days                                    |

The counter is advanced with `ADD n :one` and `SET ttl = if_not_exists(ttl, :ttl)` in one `UpdateItem` that returns the new
value, so the first create of a day creates the row and every create costs exactly one write. Two days of TTL rather than one
covers a viewer whose clock or timezone puts them a few hours ahead of UTC.

## Access patterns

| # | Need                                       | Query                                                                                   |
|---|--------------------------------------------|-----------------------------------------------------------------------------------------|
| 1 | Redirect a viewer                          | get `shorten#L#{code}` / `#METADATA`, eventually consistent                             |
| 2 | Create a link                              | put `shorten#L#{code}` conditional on `attribute_not_exists(PK)`                        |
| 3 | Read a link's rules for the manage panel   | the same get as 1, then verify the secret against `secret_hash`                         |
| 4 | Edit a link                                | update `shorten#L#{code}`, setting `url`, `rules` or both, conditional on the key existing |
| 5 | Delete a link                              | delete `shorten#L#{code}` conditional on the key existing                               |
| 6 | Stats for a window of days                 | `BatchGetItem` over `shorten#S#{code}#{date}` for each date, chunked at 100 keys        |
| 7 | Count today's creates from an IP           | update `shorten#R#{ip}#{today}` with `ADD n :one`, returning the new value              |
| 8 | Write a day of stats                       | put `shorten#S#{code}#{date}`, unconditional, replacing whatever was there              |
| 9 | Advance a link's lifetime total            | update `shorten#L#{code}` with `ADD clicks_total :total`, guarded on `last_rollup`      |

Nothing scans, nothing queries a partition range, and nothing reads an index. Nine patterns, all of them point operations.

## Flows

### Redirect (`redirect` Lambda)

This function is the CloudFront distribution's default origin, so every request that is not `/api*` arrives here.

1. The request is refused unless it carries an `X-Origin-Verify` header matching the `ORIGIN_VERIFY` environment variable.
   CloudFront adds the header; a request that reached the function URL directly does not have it. When `ORIGIN_VERIFY` is unset
   the check is skipped entirely, which is what makes the function usable under `cargo lambda watch`.
2. `GET /` answers with the single-page frontend, compiled into the binary with `include_str!`. `GET /favicon.ico` answers 204.
   The distribution has no `default_root_object` precisely so that `GET /` reaches this branch.
3. `GET /{code}` checks the code's length and character set before anything else. An invalid code answers 404 having performed
   zero DynamoDB reads, so a crawler walking a dictionary of paths costs nothing but a Lambda invocation.
4. The link row is read with a single eventually consistent `GetItem`. An absent row, or one whose `ttl` has already passed, is a
   404. Checking the `ttl` on read is what keeps DynamoDB's deletion lag — which is up to a couple of days — invisible.
5. The `s` query string is parsed into a segment. Anything that is not four non-empty pipe-separated fields, including an absent
   query string, becomes the unknown segment `XX|XX|other|other`, which matches only wildcard rules.
6. The rules are walked in order and the first whose every present dimension contains the viewer's value wins. If none does, the
   link's own `url` is the answer.
7. The response is `302` with the `Location` and `Cache-Control: public, max-age=300`. Never `301`: browsers cache a permanent
   redirect indefinitely, and these targets are editable, so a `301` would strand every viewer who ever followed the old one.

### Management API (`mgmt` Lambda)

The CloudFront `/api*` behaviour strips the `/api` prefix before the request reaches the function, so the function sees `/links`,
`/links/{code}` and `/links/{code}/stats`. Caching is disabled on that behaviour. The same `X-Origin-Verify` check as above runs
first on every route.

**`POST /links`** creates a link.

1. The IP's counter for today is advanced by one and the new value read back. Above 30 the request is refused with 429 before
   anything is validated, so a flood costs one write and no reads.
2. The body's `url` and every rule's `url` are validated, and the rule list is checked against its limits: at most 20 rules, at
   most 30 values in a dimension, uppercase country and region codes, platforms and devices drawn from their fixed sets. Any
   failure is a 422 naming the rule's position and the dimension.
3. A management secret is generated — 32 CSPRNG bytes, base64url — and its hex SHA-256 kept. The plaintext is returned in this
   response and never stored, so there is no recovery path and the frontend says so.
4. A code is generated and put conditionally on the key not existing. A failed condition means a collision: a fresh code is drawn
   and put again, immediately, up to eight times, because a new code is statistically independent of the one that collided and
   waiting would achieve nothing. A throttle, a 5xx or a network error is transient instead, and is retried after a full-jitter
   sleep of up to `min(50ms · 2^n, 3s)`. Anything else is returned to the caller.
5. The answer is `201 {code, short_url, manage_secret}`.

**`GET /links/{code}`** reads a link's `url` and `rules` after verifying the secret. It exists because the manage panel has to
show the rules before it can offer to edit them, and a `PATCH` that replaced the whole rule list from a form the user had never
seen populated would silently discard rules.

**`PATCH /links/{code}`** verifies the secret, validates whatever of `url` and `rules` the body carries, and writes them with a
single `UpdateItem` conditional on the row existing. A body carrying neither is a 422 rather than a write of nothing.

**`DELETE /links/{code}`** verifies the secret and deletes the row conditional on it existing. The stats rows are left to their
own TTL; they are keyed by code and there is nothing left to read them with.

**`GET /links/{code}/stats?from=&to=`** verifies the secret, refuses a window longer than 100 days or running backwards, builds
one key per date in the window, and reads them in chunks of 100 through `BatchGetItem`. Keys DynamoDB reports as unprocessed are
retried with the same jittered backoff, up to eight rounds, after which the request fails rather than silently returning a
partial history. Days with no row are days with no clicks and are simply absent from the answer.

Every route verifies the secret by hashing what the caller sent and comparing it to `secret_hash` in constant time. A wrong
secret is 401 and an absent link is 404; the error body carries a code and a message and never an internal detail.

### Nightly rollup (`rollup` Lambda)

EventBridge runs this function at `cron(15 2 * * ? *)`, a quarter past two in the morning UTC, for the day before. The event
payload may name a date instead, which is how a missed or suspect night is replayed.

1. An Athena query runs against the Glue table `cloudfront_logs`, restricted to that date's `year`, `month` and `day` partitions
   so partition projection prunes everything else, and to `sc_status = 302`. It extracts the code from `cs_uri_stem` and the
   segment from `cs_uri_query`, and groups by both. The workgroup caps any single query at 1 GB scanned, so a query that forgot
   its partition predicate fails rather than bills.
2. The function polls the query's state until it succeeds or fails, then reads the result rows.
3. For each code, the day's rows become one stats row: `clicks` the day's total, `seg` the per-segment counts. A row whose path
   is not a well-formed code is discarded, which is where the frontend, `/api*` and a dictionary walk all land. A row whose
   query string carries no readable segment is not discarded: its clicks count towards the code's total under the unknown
   segment `XX|XX|other|other`, because a click whose dimensions were lost is still a click. The summary and the log line
   report the two separately, as `discarded_clicks` and `unsegmented_clicks`. The row is put whole, which overwrites whatever a
   previous run wrote. That is the first half of idempotency.
4. Each link row is then advanced with `ADD clicks_total :day_total`, guarded by
   `attribute_not_exists(last_rollup) OR last_rollup < :date`, and the same update sets `last_rollup` to the date. That is the
   second half: a re-run of the same date fails the condition and adds nothing, and a link deleted since the clicks happened
   fails too and is skipped.
5. Writes are paced. The table's 15 write units are shared with `donation`, and a night's work is up to two items per active
   link landing in a burst. The function throttles itself to stay inside the shared budget and retries a throttled write with
   full jitter, because the alternative is tripping the platform's write-throttle alarm and taking `donation` down with it.

A day's clicks are therefore visible in the dashboard the morning after, and a replay of any date within the log bucket's 90-day
lifecycle reproduces exactly the same numbers.

## Retention

| Data                    | Retention                | Mechanism                                              |
|-------------------------|--------------------------|--------------------------------------------------------|
| Link rows               | 5 years from creation    | `ttl`, 1,826 days; reads treat an expired row as absent |
| Daily stats rows        | 90 days from the day     | `ttl`                                                   |
| Create rate-limit rows  | 2 days from the day      | `ttl`                                                   |
| CloudFront access logs  | 90 days                  | S3 lifecycle rule on the shared logs bucket            |
| Athena query results    | 7 days                   | S3 lifecycle rule on the shared results bucket         |

The access logs are the one place a viewer IP, User-Agent and referer exist, because CloudFront writes those fields
unconditionally. They are not scrubbed; the 90-day lifecycle is the whole mitigation, and it is a stated non-goal to do more. The
rollup reads only the status, the path and the query string, and nothing downstream of it ever sees the other fields.

## Lambdas

| Lambda     | Trigger                                                       | Role                                                            |
|------------|---------------------------------------------------------------|-----------------------------------------------------------------|
| `redirect` | CloudFront's default behaviour: `GET /`, `GET /{code}`        | serves the page, resolves a code to a target, answers 302 or 404 |
| `mgmt`     | CloudFront `/api*`, prefix stripped                            | create, read, edit, delete a link; read a stats window           |
| `rollup`   | EventBridge `cron(15 2 * * ? *)`, or a date in the payload    | Athena query for one day, stats rows and lifetime totals         |

All three are thin handlers over `crates/shared`, and all three receive `TABLE_NAME`, `METRIC_NAMESPACE`, `ORIGIN_VERIFY`,
`ATHENA_OUTPUT`, `ATHENA_WORKGROUP`, `GLUE_DATABASE`, `GLUE_TABLE` and `LOGS_BUCKET` in their environment. The `aws-cloud`
repository deploys the functions, the distribution, the edge function, the schedule, the analytics stack, the IAM and the alarms;
this repository deploys code only.

## Deliberate simplifications and their ceilings

- **The table is shared with `donation` and provisioned at the free 25 read and 25 write units**, 15 of them on the table itself,
  with no autoscaling. Fine for the redirect path, which is one eventually consistent read per cache miss and nothing else, and
  fine for creates, which are a handful a day. The pressure is the nightly rollup: it writes up to two items per active link —
  a stats row and a total update — in one burst against a budget another application is also spending, which is why it paces
  itself rather than writing as fast as the SDK will go. What breaks past a few thousand links is that the burst stops fitting
  inside any pacing that finishes before the next night, and the write-throttle alarm the platform raises belongs to `donation`.
  The upgrade path is a dedicated on-demand table for shorten alone. It was costed: at the brief's scale of 1,000 links and
  50,000 clicks a month the rollup is at most 60,000 write request units a month and the cache-missing redirects a few thousand
  read request units, which is under ten cents a month at on-demand rates — real money against a $0.35 app, and bought only when
  the pacing stops being enough. It was deliberately deferred, not overlooked.

- **Incoming query strings are discarded at the edge.** The viewer-request function overwrites the request's query string with
  `s={segment}`, so `https://short.example/aB3xK9mQ2p?utm_source=x` reaches the origin as `?s=IN|MH|android|mobile` and the
  target is followed without the `utm_source`. This is a stated non-goal of v1, not an oversight: the segment has to be in the
  cache key for the cache to be segment-aware, and in the origin request for the redirect to resolve a rule against it. It does
  not reach the access log, which is the ceiling below.
  What breaks is campaign tagging, which is exactly what a shortener is often used for. The upgrade path is to encode the
  original query alongside the segment — `s={segment}&q={percent-encoded original}` — keep only `s` in the cache key so
  fragmentation does not change, and have the redirect function append the decoded `q` to the resolved target.

- **Cache-key fragmentation is bounded by (countries × regions × 3 × 5) per code.** The cache key is the path and the single
  query string `s`, so each code has one cache entry per distinct segment that asks for it. With region included, a link shared
  across a country with twenty first-level regions has up to 300 entries; without it, fifteen. At hobby scale most of those
  entries never exist, because most codes are seen from a handful of segments. What breaks is the hit rate on a link that goes
  genuinely global: every new region-platform-device combination is a cache miss and an origin read. The lever is already built —
  `SEGMENT_INCLUDE_REGION` in `edge/segment.js` collapses the region to `XX` at the edge and divides the key space by the region
  count — at the price of region rules no longer matching anything. Flipping it is a one-line change and a function publish.

- **The segment never reaches the access log, so every click is counted unsegmented.** The viewer-request function rewrites
  `request.querystring`, which decides the cache key and what the origin receives — but CloudFront's access log records the
  query string as the viewer sent it. An ordinary click therefore logs `cs-uri-query` as `-`, and the four dimensions the
  segment design exists to produce are not in the data the rollup reads. Measured against the deployed stack on 2026-09-18: two
  probe requests sent with `?s=zzzz` and `?foo=bar` appear in the log verbatim, while every ordinary click on the same link in
  the same minute logs `-`, and the rollup's own Athena query for 2026-09-17 returned the single row
  `(/lYBtPqhqTm, -, 1)`. The design assumed a viewer-request rewrite would be logged, and it is not. What works meanwhile is
  every total: daily clicks, the 90-day window and each link's lifetime count are right, and the whole segment breakdown reads
  `XX|XX|other|other`. The upgrade path is to stop deriving the segment at the edge and take it from what CloudFront logs on its
  own: `record_fields` on the `aws-cloud` log delivery can add `c-country`, and platform and device can be derived in the query
  from `cs(User-Agent)`, which is already on every line — one Glue schema change and one rollup query change, made together.
  The alternative is CloudFront real-time logs, which is the same upgrade path as live analytics and is billed either way.

- **Analytics are next-day, not live.** A click that happens at 02:20 UTC appears in the dashboard after the following night's
  rollup, a little over twenty-four hours later. This follows directly from putting Athena, a batch engine, out of the request
  path: the alternative is a DynamoDB counter per click, which is a write per read and a hot partition on exactly the links that
  matter most. What breaks is any use that needs to watch a launch in real time. The upgrade path is a Kinesis Firehose on
  CloudFront real-time logs into a stream the rollup already knows how to aggregate, which turns next-day into next-minute and
  turns a free log delivery into a billed one.

- **Random 60-bit codes with a conditional put, not a counter.** Ten characters from a 64-symbol alphabet is 60 bits, about
  1.15 × 10^18 codes. The chance that one insert collides with an existing row is the number of rows divided by 2^60: with a
  thousand links that is 9 × 10^-16, and even with a billion rows it is about one in 1.15 × 10^9. A collision is not an error,
  it is a retry with a fresh, statistically independent code, and eight attempts make an unrecoverable create so improbable that
  the retry ceiling is there to bound the loop rather than because it will ever be reached. The counter alternative is worse in
  every respect: one item, a hard cap of 1,000 writes a second on the partition holding it, and a serialisation point in front of
  every create. Sharding the keyspace — a prefix per shard, codes drawn within a shard — does not help either, because it divides
  the available space and the occupants by the same factor and leaves the ratio, which is the collision probability, exactly
  where it was. What would actually break is the density: at 10^17 rows, a tenth of the space, collisions become common enough
  that eight retries stop being enough, and the fix is a longer code, which costs one character per six bits.

- **Three CloudWatch alarms and no canary.** `shorten-redirect-errors`, `shorten-mgmt-errors` and `shorten-rollup-failed`, the
  last of which treats missing data as breaching so a night the rollup did not run at all alarms like a night it crashed. The
  platform's ten free alarms are entirely spent by `donation` and the table's own throttle alarms, so each of these three bills
  about ten cents a month and is most of what this app costs. A fourth alarm and a canary function were both considered and
  refused on that budget: the redirect path is the whole product, and its errors alarm already watches it. What breaks is
  detection of a failure that produces no Lambda error — a redirect that answers 404 for every code because the table name is
  wrong, say, is a successful invocation. The upgrade path is a synthetic canary hitting a known code every few minutes and a
  fourth alarm on its failures, at the cost of another ten cents a month plus its invocations.

- **Rate limiting counts creates per IP per day, and nothing else.** Thirty creates from one address in a UTC day. There is no
  limit on redirects, because redirects are cached at the edge and cost nothing, and no limit on stats reads beyond the secret
  that gates them. What breaks is an abuser with a pool of addresses, for whom thirty per address is no limit at all. The
  upgrade path is CloudFront's own rate-based WAF rule in front of `/api*`, which is the right layer for it and is not free.

- **Cost.** The whole app is expected to run between zero and about fifty cents a month at the brief's scale: CloudFront
  requests, function invocations, Lambda invocations and DynamoDB capacity all sit inside free tiers; roughly 50 MB a month of
  access logs behind the 90-day lifecycle is under a cent; thirty nightly Athena queries, nearly all of them charged at the
  10 MB minimum, are about a cent together; and Glue's catalog is free. The three alarms are the largest single line at roughly
  thirty cents. A custom domain would add about fifty cents a month for the hosted zone, with the certificate free.
