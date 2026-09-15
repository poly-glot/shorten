#!/usr/bin/env bash
set -euo pipefail

. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

USAGE="usage: qa/06-load.sh --domain <distribution-domain> [--rate 20] [--duration 120] [--table aws-cloud]

Drives a steady load at one short link through the distribution and asserts the edge p99 stays
under 300 ms and that the shared DynamoDB table recorded no throttles while it ran.

Needs vegeta or k6 on PATH; skips with instructions when neither is installed.

  --domain    the CloudFront distribution domain, with or without a scheme
  --rate      requests per second, default 20
  --duration  seconds, default 120
  --table     the shared DynamoDB table, default aws-cloud"

METRIC_SETTLE_SECONDS=120
P99_CEILING_MS=300
SUCCESS_FLOOR=0.999

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

is_zero() {
    python3 -c "print('yes' if float('$1') == 0 else 'no')"
}

is_under() {
    python3 -c "print('yes' if float('$1') < float('$2') else 'no')"
}

is_at_least() {
    python3 -c "print('yes' if float('$1') >= float('$2') else 'no')"
}

throttle_sum() {
    aws cloudwatch get-metric-statistics \
        --namespace AWS/DynamoDB \
        --metric-name "$1" \
        --dimensions "Name=TableName,Value=$TABLE" \
        --start-time "$STARTED_AT" \
        --end-time "$FINISHED_AT" \
        --period 60 \
        --statistics Sum \
        --query 'sum(Datapoints[].Sum)' \
        --output text
}

parse_args "$@"
require_args DOMAIN
require_command curl
require_command python3

case "$DOMAIN" in
    http://*|https://*) BASE="${DOMAIN%/}" ;;
    *) BASE="https://${DOMAIN%/}" ;;
esac

DRIVER=""
if command -v vegeta >/dev/null 2>&1; then
    DRIVER=vegeta
elif command -v k6 >/dev/null 2>&1; then
    DRIVER=k6
fi

if [ -z "$DRIVER" ]; then
    note "skipping the load smoke: neither vegeta nor k6 is on PATH"
    note "install one and run this again:"
    note "  brew install vegeta   or   go install github.com/tsenart/vegeta/v12@latest"
    note "  brew install k6       or   https://grafana.com/docs/k6/latest/set-up/install-k6/"
    note "in CI, either runs from its own image: grafana/k6 or peterevans/vegeta"
    exit 0
fi

WORK=$(mktemp -d)

created=$(curl -gsS -X POST "$BASE/api/links" -H 'content-type: application/json' \
    -d '{"url": "https://example.com/load-smoke"}')

CODE=$(printf '%s' "$created" | json_field code)
SECRET=$(printf '%s' "$created" | json_field manage_secret)
trap cleanup EXIT

note "driver $DRIVER, $RATE rps for ${DURATION}s against $BASE/$CODE"

STARTED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)

if [ "$DRIVER" = vegeta ]; then
    printf 'GET %s/%s\n' "$BASE" "$CODE" \
        | vegeta attack -rate "$RATE" -duration "${DURATION}s" -redirects=-1 -timeout 10s \
        >"$WORK/results.bin"

    vegeta report -type=json "$WORK/results.bin" >"$WORK/report.json"

    measured=$(python3 -c '
import json
import sys

report = json.load(open(sys.argv[1]))
print("%.1f" % (report["latencies"]["99th"] / 1e6), "%.4f" % report["success"])
' "$WORK/report.json")

    p99_ms=${measured% *}
    success=${measured#* }

    note "edge p99 $p99_ms ms, success ratio $success"
    assert_eq "yes" "$(is_under "$p99_ms" "$P99_CEILING_MS")" \
        "the edge p99 stayed under $P99_CEILING_MS ms (measured $p99_ms ms)"
    assert_eq "yes" "$(is_at_least "$success" "$SUCCESS_FLOOR")" \
        "the edge answered every request (success ratio $success)"
else
    cat >"$WORK/load.js" <<'JS'
import http from 'k6/http';
import { check } from 'k6';

export const options = {
    scenarios: {
        steady: {
            duration: `${__ENV.DURATION}s`,
            executor: 'constant-arrival-rate',
            preAllocatedVUs: 50,
            rate: Number(__ENV.RATE),
            timeUnit: '1s',
        },
    },
    thresholds: {
        checks: [`rate>=${__ENV.SUCCESS_FLOOR}`],
        http_req_duration: [`p(99)<${__ENV.P99_CEILING_MS}`],
    },
};

export default function () {
    const response = http.get(__ENV.TARGET, { redirects: 0 });
    check(response, { 'answered 302': (r) => r.status === 302 });
}
JS

    if DURATION="$DURATION" P99_CEILING_MS="$P99_CEILING_MS" RATE="$RATE" \
        SUCCESS_FLOOR="$SUCCESS_FLOOR" TARGET="$BASE/$CODE" k6 run "$WORK/load.js"; then
        pass "k6 held p99 under $P99_CEILING_MS ms and answered every request with a 302"
    else
        fail "k6 breached a threshold: p99 reached $P99_CEILING_MS ms, or a request was not a 302"
    fi
fi

FINISHED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)

require_command aws
note "waiting ${METRIC_SETTLE_SECONDS}s for DynamoDB throttle metrics to reach CloudWatch"
sleep "$METRIC_SETTLE_SECONDS"

for metric in ReadThrottleEvents WriteThrottleEvents; do
    assert_eq "yes" "$(is_zero "$(throttle_sum "$metric")")" \
        "the shared table recorded no $metric while the load ran"
done

finish
