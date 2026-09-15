#!/usr/bin/env bash
set -euo pipefail

. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

USAGE="usage: qa/03-log-segment.sh --domain <distribution-domain> [--account <id>] [--wait <seconds>]

Drives traffic through the distribution, waits for the access log to be delivered, prints the
cs-uri-query field of a real log line verbatim, and asserts the rollup's parser handles the form
that actually arrived rather than the form the design assumed.

  --domain   the CloudFront distribution domain, with or without a scheme
  --account  the AWS account id; read from STS when omitted
  --wait     seconds to wait for log delivery, default 1200"

REQUESTS=5

CODE=""
SECRET=""
WORK=""

cleanup() {
    if [ -n "$WORK" ]; then
        rm -rf "$WORK"
    fi

    [ -n "$CODE" ] || return 0

    curl -gsS -o /dev/null -X DELETE -H "x-manage-secret: $SECRET" "$BASE/api/links/$CODE" || true
}

candidate_keys() {
    aws s3api list-objects-v2 --bucket "$BUCKET" --prefix "$1" \
        --query 'reverse(sort_by(Contents, &LastModified))[:40].Key' --output text 2>/dev/null || true
}

search_prefix() {
    local key
    for key in $(candidate_keys "$1"); do
        [ "$key" = "None" ] && continue

        aws s3 cp "s3://$BUCKET/$key" "$WORK/object.gz" --quiet || continue
        gunzip -cf "$WORK/object.gz" >"$WORK/object.log" 2>/dev/null || continue

        if grep -q "/$CODE" "$WORK/object.log"; then
            cp "$WORK/object.log" "$WORK/hit.log"
            printf '%s' "$key" >"$WORK/hit.key"
            return 0
        fi
    done

    return 1
}

parse_args "$@"
require_args DOMAIN
require_command curl
require_command python3
discover_account

BUCKET="aws-cloud-logs-$ACCOUNT"

case "$DOMAIN" in
    http://*|https://*) BASE="${DOMAIN%/}" ;;
    *) BASE="https://${DOMAIN%/}" ;;
esac

WORK=$(mktemp -d)
trap cleanup EXIT

created=$(curl -gsS -X POST "$BASE/api/links" -H 'content-type: application/json' \
    -d '{"url": "https://example.com/log-format-probe"}')

CODE=$(printf '%s' "$created" | json_field code)
SECRET=$(printf '%s' "$created" | json_field manage_secret)
note "code $CODE"

attempt=0
while [ "$attempt" -lt "$REQUESTS" ]; do
    status=$(status_of "$BASE/$CODE")
    assert_eq "302" "$status" "traffic request $((attempt + 1)) redirected"
    attempt=$((attempt + 1))
done

prefixes=$(python3 -c '
import datetime
import sys

now = datetime.datetime.now(datetime.timezone.utc)
for offset in (0, 1):
    day = now - datetime.timedelta(days=offset)
    print("AWSLogs/%s/CloudFront/%s" % (sys.argv[1], day.strftime("%Y/%m/%d/")))
' "$ACCOUNT")

note "waiting up to ${WAIT}s for a delivered log object under s3://$BUCKET/AWSLogs/$ACCOUNT/CloudFront/"

deadline=$(( $(date -u +%s) + WAIT ))
found=no
while [ "$(date -u +%s)" -lt "$deadline" ]; do
    for prefix in $prefixes; do
        if search_prefix "$prefix"; then
            found=yes
            break
        fi
    done

    if [ "$found" = yes ]; then
        break
    fi

    sleep 30
done

if [ "$found" != yes ]; then
    fail "no delivered log object mentioned $CODE within ${WAIT}s; CloudFront standard logging can lag, retry with a longer --wait"
    finish
fi

note "log object $(cat "$WORK/hit.key")"

python3 - "$WORK/hit.log" "$CODE" <<'PY' || fail "the delivered cs-uri-query is not a form the rollup parses"
import sys

path, code = sys.argv[1], sys.argv[2]

fields = None
line = None

with open(path, encoding="utf-8", errors="replace") as handle:
    for raw in handle:
        raw = raw.rstrip("\n")
        if raw.startswith("#Fields:"):
            fields = raw.split(":", 1)[1].split()
            continue
        if raw.startswith("#") or not raw:
            continue
        if "/" + code in raw:
            line = raw.split("\t")
            break

if fields is None:
    sys.exit("FAIL  the log object carried no #Fields: header, so no column could be named")
if line is None:
    sys.exit("FAIL  the log object mentioned the code but no data line matched it")

index = dict((name, position) for position, name in enumerate(fields))
for name in ("cs-uri-stem", "cs-uri-query", "sc-status"):
    if name not in index:
        sys.exit("FAIL  the delivered log format has no %s field" % name)

stem = line[index["cs-uri-stem"]]
query = line[index["cs-uri-query"]]
status = line[index["sc-status"]]

print("      cs-uri-stem  %r" % stem)
print("      cs-uri-query %r" % query)
print("      sc-status    %r" % status)

failures = 0


def check(condition, message):
    global failures
    if condition:
        print("ok    " + message)
    else:
        print("FAIL  " + message, file=sys.stderr)
        failures += 1


check(stem == "/" + code, "cs-uri-stem is the bare path the rollup strips a leading slash from")
check(status == "302", "the redirect is logged with sc-status 302, which is the rollup's only filter")

check(query.startswith("s="), "cs-uri-query is exactly the s= parameter the edge function wrote")

segment = query[2:] if query.startswith("s=") else query
spelling = "percent-encoded %7C" if "%7C" in segment or "%7c" in segment else "literal |"
print("      separator arrived as: " + spelling)

if "%" in segment:
    segment = segment.replace("%7C", "|").replace("%7c", "|")

parts = segment.split("|")
check(len(parts) == 4 and all(parts),
      "the rollup's decode-then-split yields four non-empty dimensions from the delivered form")

sys.exit(1 if failures else 0)
PY

finish
