#!/usr/bin/env bash
set -euo pipefail

. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

USAGE="usage: qa/03-log-segment.sh --domain <distribution-domain> [--account <id>] [--wait <seconds>]

Drives traffic through the distribution, waits for the access log to be delivered, prints the
cs-uri-query field of a real log line verbatim, and asserts the rollup reads what actually
arrives rather than what the design assumed.

What it pins down is that CloudFront logs the query string the VIEWER sent, not the one the
viewer-request function wrote: an ordinary click logs '-', so the segment is never in the log and
every click is counted under the unknown segment. One request carries a query string of its own,
which the log does record, so both cases sit side by side in one object.

  --domain   the CloudFront distribution domain, with or without a scheme
  --account  the AWS account id; read from STS when omitted
  --wait     seconds to wait for log delivery, default 1200"

PROBE_QUERY="probe=03"
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

status=$(status_of "$BASE/$CODE?$PROBE_QUERY")
assert_eq "302" "$status" "the probe request, carrying a query string of its own, redirected"

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

python3 - "$WORK/hit.log" "$CODE" "$PROBE_QUERY" <<'PY' || fail "the delivered log is not what the rollup reads"
import sys

path, code, probe_query = sys.argv[1], sys.argv[2], sys.argv[3]

UNKNOWN_SEGMENT = "XX|XX|other|other"

with open(path, encoding="utf-8", errors="replace") as handle:
    lines = [line for line in handle.read().split("\n") if line]

headers = [line for line in lines if line.startswith("#Fields:")]
if not headers:
    sys.exit("FAIL  the log object carried no #Fields: header, so no column could be named")

fields = headers[0].split(":", 1)[1].split()
index = dict((name, position) for position, name in enumerate(fields))
for name in ("cs-uri-stem", "cs-uri-query", "sc-status"):
    if name not in index:
        sys.exit("FAIL  the delivered log format has no %s field" % name)

STEM, QUERY, STATUS = index["cs-uri-stem"], index["cs-uri-query"], index["sc-status"]
WIDTH = max(STEM, QUERY, STATUS)

data = [line.split("\t") for line in lines if not line.startswith("#")]
clicks = [line for line in data if len(line) > WIDTH and line[STEM] == "/" + code]
if not clicks:
    sys.exit("FAIL  the log object mentioned the code but no data line requested /%s" % code)

plain = [line for line in clicks if line[QUERY] != probe_query]
probed = [line for line in clicks if line[QUERY] == probe_query]


def segment_of(query):
    if not query.startswith("s="):
        return None

    segment = query[2:]
    if "%" in segment:
        segment = segment.replace("%7C", "|").replace("%7c", "|")

    parts = segment.split("|")

    return segment if len(parts) == 4 and all(parts) else None


def segment_or_unknown(query):
    return segment_of(query) or UNKNOWN_SEGMENT


print("      cs-uri-stem   %r" % clicks[0][STEM])
print("      cs-uri-query  %r  (an ordinary click)" % (plain[0][QUERY] if plain else None))
print("      cs-uri-query  %r  (the probe click, which sent ?%s)" % (probed[0][QUERY] if probed else None, probe_query))

failures = 0


def check(condition, message):
    global failures
    if condition:
        print("ok    " + message)
    else:
        print("FAIL  " + message, file=sys.stderr)
        failures += 1


check(all(line[STATUS] == "302" for line in clicks),
      "every logged click is sc-status 302, which is the rollup's only filter")
check(bool(plain), "an ordinary click reached the log")
check(bool(probed), "the probe click reached the log")
check(bool(probed) and probed[0][QUERY] == probe_query,
      "cs-uri-query is the query string the viewer sent, recorded verbatim")
check(bool(plain) and segment_of(plain[0][QUERY]) is None,
      "an ordinary click carries no segment: the edge function's querystring rewrite is not logged")
check(bool(plain) and segment_or_unknown(plain[0][QUERY]) == UNKNOWN_SEGMENT,
      "the rollup counts that click under the unknown segment rather than discarding it")

sys.exit(1 if failures else 0)
PY

finish
