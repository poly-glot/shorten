#!/usr/bin/env bash
set -euo pipefail

. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

USAGE="usage: qa/02-edge-cache.sh --domain <distribution-domain> [--verify <origin-secret>]

Creates a link whose rules make the resolved target reveal the segment CloudFront derived,
then requests it through the distribution twice per segment.

  --domain   the CloudFront distribution domain, with or without a scheme
  --verify   only needed when the link is created on the mgmt origin instead of /api"

ANDROID_UA='Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Mobile Safari/537.36'
DESKTOP_UA='Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36'

ANDROID_URL="https://example.com/android"
DESKTOP_URL="https://example.com/desktop"
CLASSIFIED_URL="https://example.com/classified-but-neither"
UNKNOWN_COUNTRY_URL="https://example.com/no-country-header"

CODE=""
SECRET=""

cleanup() {
    [ -n "$CODE" ] || return 0

    curl -gsS -o /dev/null -X DELETE \
        -H "x-manage-secret: $SECRET" \
        "$BASE/api/links/$CODE" || true
}

parse_args "$@"
require_args DOMAIN
require_command curl
require_command python3

case "$DOMAIN" in
    http://*|https://*) BASE="${DOMAIN%/}" ;;
    *) BASE="https://${DOMAIN%/}" ;;
esac

created=$(curl -gsS -X POST "$BASE/api/links" \
    -H 'content-type: application/json' \
    -d @- <<JSON
{
    "url": "$CLASSIFIED_URL",
    "rules": [
        { "countries": ["XX"], "url": "$UNKNOWN_COUNTRY_URL" },
        { "platforms": ["android"], "url": "$ANDROID_URL" },
        { "devices": ["desktop"], "url": "$DESKTOP_URL" }
    ]
}
JSON
)

CODE=$(printf '%s' "$created" | json_field code)
SECRET=$(printf '%s' "$created" | json_field manage_secret)
trap cleanup EXIT

short_url=$(printf '%s' "$created" | json_field short_url)
note "code $CODE"
assert_contains "$BASE" "$short_url" "short_url points at the distribution, not at a raw lambda-url origin"

probe() {
    capture -H "user-agent: $1" "$BASE/$CODE"

    PROBE_CACHE=$(captured_header x-cache)
    PROBE_STATUS=$CAPTURED_STATUS
    PROBE_TARGET=$(captured_header location)
}

for attempt in first second; do
    probe "$ANDROID_UA"
    android_cache=$PROBE_CACHE
    android_status=$PROBE_STATUS
    android_target=$PROBE_TARGET

    probe "$DESKTOP_UA"
    desktop_cache=$PROBE_CACHE
    desktop_status=$PROBE_STATUS
    desktop_target=$PROBE_TARGET

    note "$attempt pass: android x-cache [$android_cache], desktop x-cache [$desktop_cache]"
done

assert_eq "302" "$android_status" "the distribution answers an android viewer with a 302"
assert_eq "302" "$desktop_status" "the distribution answers a desktop viewer with a 302"

assert_eq "$ANDROID_URL" "$android_target" \
    "CloudFront-Is-Android-Viewer reached the edge function: an android viewer resolves to the android rule"
assert_eq "$DESKTOP_URL" "$desktop_target" \
    "CloudFront-Is-Desktop-Viewer reached the edge function: a desktop viewer resolves to the desktop rule"

if [ "$android_target" = "$UNKNOWN_COUNTRY_URL" ] || [ "$desktop_target" = "$UNKNOWN_COUNTRY_URL" ]; then
    fail "the country dimension came back XX: CloudFront-Viewer-Country did not reach the edge function"
else
    pass "CloudFront-Viewer-Country reached the edge function: the country dimension is not XX"
fi

assert_eq "Hit from cloudfront" "$android_cache" "the second android request is served from the edge cache"
assert_eq "Hit from cloudfront" "$desktop_cache" "the second desktop request is served from the edge cache"

finish
