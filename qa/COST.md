# Cost report

The brief's target is under a dollar a month at 1,000 links and 50,000 redirects. The expected bill is
**$0 to $0.50 a month**, and almost all of it is three CloudWatch alarms. `qa/07-cost.sh` asserts, against
the deployed account, every structural claim this report rests on; this file is the arithmetic behind it.

Scale assumed throughout: 1,000 live links, 50,000 redirects a month, a handful of creates a day, one
rollup a night. The `shorten` app shares its account and its DynamoDB table with `donation`, so several
lines below are shared allowances rather than exclusively shorten's.

## What the traffic costs

| Service | Monthly usage at the brief's scale | Free allowance | Billed |
|---|---|---|---|
| CloudFront requests | ~50k HTTPS requests, a few MB out | 10M requests, 1 TB out, always free | $0 |
| CloudFront Functions | ~50k `shorten-segment` invocations, one per request | 2M invocations, always free | $0 |
| Lambda | ~5k `shorten-redirect` invocations after cache hits, plus 30 rollup runs | 1M invocations, 400k GB-seconds, always free | $0 |
| DynamoDB | ~5k eventually consistent reads, ~2k rollup writes, ~100 create writes | 25 provisioned read and 25 write units, always free | $0 |
| S3, access logs | ~50 MB behind a 90-day lifecycle | none after the first 12 months | under $0.01 |
| S3, Athena results | emptied by a 7-day lifecycle | none after the first 12 months | under $0.01 |
| Athena | 30 nightly queries, nearly all charged at the 10 MB minimum | none | about $0.01 |
| Glue Data Catalog | one database, one table, no crawler | 1M objects stored, 1M requests | $0 |
| EventBridge scheduler | 30 invocations | free for AWS-service targets | $0 |
| CloudWatch Logs | well under a gigabyte ingested | 5 GB ingested, 5 GB stored | $0 |
| **CloudWatch alarms** | **three** | **10 per account, and `donation` plus the table's throttle alarms already use all ten** | **about $0.30** |
| CloudWatch custom metrics | none; shorten emits no EMF metric | 10 per account, all spent by `donation` | $0 |

**Total: $0.30 to $0.35 a month**, rounded up to $0.50 to leave room for a month with unusual traffic.

## Why the cache is the cost model

A redirect that hits the CloudFront cache costs one CloudFront request and one CloudFront Function
invocation, and nothing else: no Lambda, no DynamoDB read. Both of those sit inside allowances that are
always free rather than free for twelve months, so redirect volume is genuinely free until it passes ten
million requests a month.

The cache key is the URL path plus the single query string `s`, so a link has one cache entry per distinct
viewer segment. Clicks that miss cost one Lambda invocation and one eventually consistent `GetItem`, a
half read unit. At the brief's scale that is a few thousand reads a month against 15 provisioned units
that are already paid for at zero.

Counting clicks from the edge access log rather than from a DynamoDB counter is what keeps this true. A
counter would make every redirect a write, would put every click on one hot partition, and would reach the
free 15 write units at roughly one click a second.

## What actually bills

**Three CloudWatch alarms, about $0.10 each.** `shorten-redirect-errors`, `shorten-mgmt-errors` and
`shorten-rollup-failed`. The account's ten free alarms are entirely spent by `donation`'s eight and the
shared table's two throttle alarms, so each of shorten's three is billed in full. This is the single
largest line in the app and the reason a fourth alarm and a synthetic canary were both refused.

**The analytics stack, $0.02 to $0.10 a month**, shared with anything else that ever logs into the same
bucket. About 50 MB of gzipped access logs behind a 90-day lifecycle, an Athena results bucket emptied
every seven days, and thirty nightly queries whose partition predicates keep each one at or near Athena's
10 MB minimum charge of $0.00005. The workgroup caps any single query at 1 GB scanned, so a query that
lost its partition predicate fails rather than bills; the cap is enforced at the workgroup so a client
cannot opt out of it.

**S3 storage, under a cent.** S3's own free tier lasts twelve months, so after the first year the log and
results buckets bill at standard rates on a few tens of megabytes.

Everything else is inside an always-free allowance at this scale.

## Free-tier honesty

A new AWS account no longer gets the old twelve-month free tier. It gets **credits** — a $100 sign-up
credit plus up to $100 more earned by completing activities — and those credits pay for whatever the
always-free allowances do not cover, until they expire or are spent. That means a new account sees a
**$0.00 invoice** for this app for a long while and learns nothing about what it costs. The
`aws-cloud-monthly` budget is the instrument that tells the truth: it notifies at 80% of $5, at $5, and
when the month's forecast passes $5. `qa/07-cost.sh` asserts the budget exists and has at least one
subscriber, because a budget nobody is notified by is decoration.

The always-free allowances in the table above — CloudFront's 10M requests and 2M function invocations,
Lambda's 1M invocations and 400k GB-seconds, DynamoDB's 25 provisioned units, Glue's catalog, CloudWatch's
10 alarms and 5 GB of logs — do not expire and are not credits. They are what makes the steady-state bill
thirty cents rather than several dollars.

## What would change the bill

| Change | New cost | Why |
|---|---|---|
| A custom domain | about +$0.50/month | a Route 53 hosted zone; the ACM certificate is free |
| A fourth alarm or a canary | +$0.10/month each, plus canary invocations | the free ten are gone |
| A dedicated on-demand DynamoDB table for shorten | about +$0.10/month at this scale | the named upgrade path when the rollup's pacing stops fitting the night |
| Real-time analytics via Kinesis Firehose | dollars, not cents | turns a free log delivery into a billed stream |
| Traffic past 10M requests a month | CloudFront per-request and per-GB rates | the always-free CloudFront allowance is the widest margin here |
| Parquet conversion on the access logs | CloudWatch Logs charges per GB converted | deliberately not used; at MB a day it buys nothing |

## The checklist, and what asserts it

`qa/07-cost.sh` makes each of these a real AWS call with an assertion:

| Claim | Assertion |
|---|---|
| The table is the shared one | all three functions' `TABLE_NAME` equals the shared table name |
| On-demand is not in use | `BillingModeSummary.BillingMode` is `PROVISIONED` |
| No provisioned capacity beyond the free 25 | table plus both indexes sum to exactly 25 read and 25 write units |
| No autoscaling | no DynamoDB scalable target exists; each policy would create billed alarms |
| No VPC | every function's `VpcConfig.VpcId` is absent |
| No NAT gateway | no NAT gateway in the account is pending or available |
| The 90-day log lifecycle is active | the logs bucket has an enabled rule expiring at 90 days |
| The Athena 1 GB scan cap is enforced | `BytesScannedCutoffPerQuery` is 1073741824 and `EnforceWorkGroupConfiguration` is true |
| The $5 budget alarm exists | `aws-cloud-monthly` has a limit of 5 USD and at least one notification |
| Three alarms and no fourth | exactly three alarms carry the `shorten-` prefix |

None of these has been run against real infrastructure: nothing is deployed yet. The numbers above are a
model, and the script is what turns the model into a measurement on the day there is an account to measure.
