# QA against a deployed stack

Seven scripts. Each one takes its inputs on the command line, prints one `ok` or `FAIL` line per
assertion, and **exits non-zero if any assertion failed** — an exit code of 1 is a broken deployment, 2 is
a broken invocation (a missing argument, no credentials, a tool that is not installed).

They exist because five things about this design cannot be settled anywhere but a real deploy, and four of
the five fail *silently*: the redirects keep working, the dashboard stays empty, and nothing errors
anywhere. The open questions are listed at the end of this file with the script that closes each.

**`04-log-prefix.sh` has been run against the deployed stack, on 2026-09-18, and passes.** The rest have
not: they are written and syntax-checked, their argument handling and their offline parsing paths have been
exercised, and their AWS and HTTP paths have not.

## Running them

`bash`, `curl`, `python3` and the AWS CLI v2 with credentials for the deployed account. No jq, no Node, no
Go. `qa/06-load.sh` additionally wants `vegeta` or `k6`, and skips cleanly when neither is present.

```bash
export DOMAIN=d111111abcdef8.cloudfront.net
export VERIFY=<the SHORTEN_ORIGIN_VERIFY value>

qa/01-routing.sh    --verify "$VERIFY"
qa/02-edge-cache.sh --domain "$DOMAIN"
qa/03-log-segment.sh --domain "$DOMAIN"
qa/04-log-prefix.sh
qa/05-negative.sh   --verify "$VERIFY"
qa/06-load.sh       --domain "$DOMAIN"
qa/07-cost.sh
```

That runs the whole suite in the order the failures are most useful in: routing before caching, caching
before logs, logs before cost. `01` and `05` read the two function URLs from
`aws lambda get-function-url-config` when `--mgmt-url` and `--redirect-url` are not given.

In CI, run them under `set -e` in that order and let the first non-zero exit stop the job.

`SHORTEN_ORIGIN_VERIFY` is a repository secret on `aws-cloud`, not on this repository, and it is not
readable back out of GitHub. Take it from wherever it was generated, or rotate it and use the new value
(see the runbook in the top-level `README.md`).

## What each script proves, and what a failure means

### `01-routing.sh` — the rule engine, over the wire

Creates one link with three rules and then hits the **redirect function URL directly**, bypassing
CloudFront, with `s=` values written by hand. Bypassing the edge is the point: it is the only way to
choose a segment, because the edge function overwrites whatever query string a client sends.

| Assertion | What a failure means |
|---|---|
| a resolved code answers 302 with `Cache-Control: public, max-age=300` | a 301 would be cached by browsers forever and strand every viewer when the rules change |
| `IN\|MH\|android\|mobile` takes the android rule, not the later mobile rule | first-match precedence is broken; rule order no longer means anything |
| `PK\|SD\|android\|desktop` takes the android rule | OR-within-a-dimension is broken; only the first value of a list matches |
| `IN\|MH\|ios\|mobile` falls through to the mobile rule | AND-across-dimensions is broken; a rule matches on any dimension rather than all of them |
| `DE\|BE\|other\|mobile` takes the device-only rule | an omitted dimension has stopped being a wildcard |
| `US\|CA\|ios\|tablet` takes the tablet rule | `tablet` is being treated as a kind of `mobile` |
| `US\|NY\|ios\|tablet` falls to the default | the region dimension is not being evaluated |
| `XX\|XX\|other\|other` falls to the default | the guaranteed fallback is not guaranteed |
| `s=IN%7CMH%7Candroid%7Cmobile` resolves like the literal form | the redirect's percent-decode is broken, which matters because CloudFront may deliver either spelling |

The link is deleted on exit, including on failure.

### `02-edge-cache.sh` — the edge function and the cache

Closes open questions 3 and 4. Requests go through the distribution, so the segment is whatever CloudFront
derived from the viewer — which is exactly what needs testing.

The trick is the rule list. The link's first rule matches `countries: ["XX"]`, the second `platforms:
["android"]`, the third `devices: ["desktop"]`, and the default URL means "classified, but neither".
Because the resolved target differs per branch, the `Location` header reports the segment CloudFront
actually computed, without needing to read a log.

| Assertion | What a failure means |
|---|---|
| `short_url` names the distribution | `PUBLIC_BASE_URL` is empty: every link handed out bypasses CloudFront, so no cache, no segment, no log line, no analytics, and nothing errors. See the two post-apply steps in the top-level `README.md` |
| an Android User-Agent resolves to the android rule | `CloudFront-Is-Android-Viewer` is not reaching the viewer-request function; every viewer segments as `other` and every platform rule silently stops matching |
| a desktop User-Agent resolves to the desktop rule | the same, for the device dimension |
| neither resolves to the `countries: ["XX"]` rule | `CloudFront-Viewer-Country` is not reaching the function: every viewer is `XX`, every country rule silently stops matching, and the analytics go flat. This is **open question 3**, and it is why asserting "a 302 came back" would not do |
| the second request per segment carries `x-cache: Hit from cloudfront` | the cache key is not what the cache policy says, or the edge function is not deterministic. This is **open question 4** |

Two caveats worth knowing before reading a failure: the country assertion is only meaningful if the
machine running it has a geolocatable address, and the cache assertion assumes both requests reached the
same edge location, which is normal from one host but not guaranteed.

### `03-log-segment.sh` — whose query string the log records

Closes **open question 1**, whose answer turned out to make the question moot. Creates a link, drives five
plain redirects and one carrying `?probe=03` through the distribution, then polls the logs bucket until a
delivered object mentions that code, and prints the `cs-uri-query` field of both **verbatim**.

It reads the column positions from the file's own `#Fields:` header rather than assuming an order, matches
on the `cs-uri-stem` column so an `/api*` line for the same code cannot be mistaken for a click, then
applies the rollup's own parse — strip `s=`, percent-decode `%7C` and `%7c` only when a `%` is present,
split on `|` — and the rollup's own fallback when that yields nothing.

| Assertion | What a failure means |
|---|---|
| `cs-uri-stem` is `/{code}` | the rollup's `strip_prefix('/')` will not find the code |
| `sc-status` is `302` | the rollup filters on `sc_status = 302` and would drop every row |
| the probe click's `cs-uri-query` is exactly what the viewer sent | the log is not a record of the viewer's request, and everything below is reasoning about the wrong data |
| an ordinary click's `cs-uri-query` carries no segment | **AWS changed something**: the viewer-request function's querystring rewrite has started reaching the log, which would mean real segments are available again and the ceiling in `specs/01-dynamodb-data-model.md` should be revisited and closed |
| the rollup counts that click under `XX\|XX\|other\|other` | a click whose dimensions were lost is being discarded rather than counted, which is the bug of 2026-09-18: totals read zero and nothing errors |

The last two are the point of the script. The segment the edge function derives is **not** in the access
log and never was — CloudFront records the query string the viewer sent — so the dimensions are lost and
only the totals survive. "the parser is broken", "the segment never arrived" and "there was no traffic"
look identical from a dashboard, and this is what tells them apart.

Log delivery lags. The default wait is twenty minutes (`--wait 1200`); CloudFront's documented worst case
is longer, so a failure here should be retried with a larger `--wait` before it is believed.

### `04-log-prefix.sh` — the delivered S3 prefix

Closes **open question 2**, and needs no traffic of its own — it only needs one delivered object.

`aws s3 ls` the logs bucket, then assert the objects sit under `AWSLogs/<account>/CloudFront/`, that the
Glue table's `location` names that same prefix, that `storage.location.template` continues it with
`${year}/${month}/${day}`, and that a real delivered key continues `yyyy/MM/dd` with the digit widths the
projection declares.

A failure means partition projection matches nothing, every rollup query returns zero rows, and the
dashboard is empty with no error anywhere. The fix is one line: `local.cloudfront_log_prefix` in
`aws-cloud/terraform/modules/analytics/main.tf`.

If the bucket is empty the script fails rather than passes — an empty bucket cannot confirm a prefix.

### `05-negative.sh` — the refusals

Hits the origins directly so the origin-verify behaviour can be observed at all.

| Assertion | What a failure means |
|---|---|
| the real secret reads the link | the refusals below are refusing everything rather than refusing the right things |
| a wrong or absent `X-Manage-Secret` → 401 | anybody can edit or delete anybody's link |
| an unknown but well-formed code → 404, on both functions | an enumerator learns which guesses were close |
| a malformed code → 404 on the redirect path | the charset fast-reject is gone, so a dictionary walk now costs a table read per path |
| `javascript:` and `http://169.254.169.254/` → 422 | the URL validator is not refusing scheme abuse or metadata-endpoint targets |
| a twenty-first rule → 422 | the rule ceiling is not enforced |
| no or wrong `X-Origin-Verify` → **403 from redirect, 404 from mgmt** | origin protection is off, and the function URLs are directly reachable |

The two different status codes are deliberate and documented in `aws-cloud`'s README: the redirect path
refuses loudly because its only caller is CloudFront; the management API prefers to give a prober nothing
that distinguishes "wrong header" from "no such route". A monitor that assumes one code for both is wrong.

There is no 429 test. The limit is 30 creates per IP per UTC day and tripping it would consume the whole
day's budget for the address running CI, leaving every other script unable to create its fixture. The
suite creates about four links per full run, so a day holds roughly seven runs from one address.

### `06-load.sh` — the load smoke

20 rps for 2 minutes against one short link through the distribution, then the throttle metrics.

Neither `vegeta` nor `k6` is installed on the machine this was written on, so the script **detects and
skips with instructions** rather than failing: a missing load generator is not a broken deployment.
Install one with `brew install vegeta` or `brew install k6`, with `go install
github.com/tsenart/vegeta/v12@latest`, or in CI from the `peterevans/vegeta` or `grafana/k6` images.

Under vegeta the script parses the JSON report and asserts `p99 < 300 ms` and a success ratio at or above
0.999. Under k6 the same two bars are expressed as k6 thresholds, so k6's own non-zero exit is the
assertion. Both drivers are told not to follow the 302 — following it would measure example.com.

Then it waits two minutes for metrics to settle and asserts `ReadThrottleEvents` and `WriteThrottleEvents`
on the shared table both summed to zero over the window. A throttle here is a `donation` problem as much
as a shorten one: the 25 provisioned units are shared, and the table's throttle alarms are `donation`'s.

At 20 rps with one link and one segment the cache absorbs nearly everything, so a throttle at this rate
means the cache is not hitting — which `02-edge-cache.sh` would have already said.

### `07-cost.sh` — the cost checklist

Ten assertions, each a real AWS call. The full list and the arithmetic they support are in
[`COST.md`](COST.md). A failure is a bill, not an outage: on-demand billing silently enabled, an
autoscaling policy quietly adding billed alarms, a lifecycle rule that lapsed, a workgroup whose scan cap
stopped being enforced, a fourth alarm.

## The five open questions

| # | Question | Closed by |
|---|---|---|
| 1 | ~~Does `\|` reach the log as a literal or as `%7C`?~~ **Answered 2026-09-18: neither.** CloudFront logs the query string the viewer sent, so the segment never reaches the log at all and every click counts as unsegmented | `03-log-segment.sh`, which now prints an ordinary click's `cs-uri-query` and a probe click's side by side, and asserts the rollup counts rather than discards the first |
| 2 | Is the delivered S3 prefix the one Glue projects? | `04-log-prefix.sh`, against both the objects and the table's own projection template |
| 3 | Do the `CloudFront-*` headers reach the viewer-request function? | `02-edge-cache.sh`, by making the resolved target reveal the derived segment — including a non-`XX` country |
| 4 | Does the second request per segment hit the cache? | `02-edge-cache.sh`, asserting `x-cache: Hit from cloudfront` |
| 5 | Are `date` and `time` backticked in every Athena query? | nothing here: an unbackticked query fails loudly rather than silently, so it is a papercut. The rollup's own query selects neither column. It is recorded in the runbooks in the top-level `README.md` |

## Housekeeping

Every script creates its fixtures through the API and deletes them in an `EXIT` trap, so a failure leaves
nothing behind but the stats rows a real click produced, which carry their own 90-day TTL.

Creates count against the per-IP daily limit; redirects do not. Nothing here writes to the table outside
the API, and nothing reads another application's rows: the IAM fence is `dynamodb:LeadingKeys` on
`shorten#*`.
