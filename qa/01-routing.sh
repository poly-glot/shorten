#!/usr/bin/env bash
set -euo pipefail

. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

USAGE="usage: qa/01-routing.sh --verify <origin-secret> [--mgmt-url <url>] [--redirect-url <url>]

Creates one link with three rules on the mgmt origin, then hits the redirect origin
directly with hand-crafted s= values and asserts every routing branch.

  --verify        the value of SHORTEN_ORIGIN_VERIFY, sent as X-Origin-Verify
  --mgmt-url      shorten-mgmt function URL; read from AWS when omitted
  --redirect-url  shorten-redirect function URL; read from AWS when omitted"

DEFAULT_URL="https://example.com/default"
MOBILE_URL="https://example.com/mobile"
PLAY_URL="https://play.google.com/store/apps/details?id=com.example.app"
TABLET_URL="https://example.com/tablet-in-california"

CODE=""
SECRET=""

cleanup() {
    [ -n "$CODE" ] || return 0

    curl -gsS -o /dev/null -X DELETE \
        -H "x-origin-verify: $VERIFY" \
        -H "x-manage-secret: $SECRET" \
        "$MGMT_URL/links/$CODE" || true
}

resolved() {
    location_of -H "x-origin-verify: $VERIFY" "$REDIRECT_URL/$CODE?s=$1"
}

parse_args "$@"
require_args VERIFY
require_command python3
discover_origins

created=$(curl -gsS -X POST "$MGMT_URL/links" \
    -H "x-origin-verify: $VERIFY" \
    -H 'content-type: application/json' \
    -d @- <<JSON
{
    "url": "$DEFAULT_URL",
    "rules": [
        { "countries": ["IN", "PK"], "platforms": ["android"], "url": "$PLAY_URL" },
        { "devices": ["mobile"], "url": "$MOBILE_URL" },
        { "devices": ["tablet"], "regions": ["CA"], "url": "$TABLET_URL" }
    ]
}
JSON
)

CODE=$(printf '%s' "$created" | json_field code)
SECRET=$(printf '%s' "$created" | json_field manage_secret)
trap cleanup EXIT

note "code $CODE"
note "short_url $(printf '%s' "$created" | json_field short_url)"

capture -H "x-origin-verify: $VERIFY" "$REDIRECT_URL/$CODE?s=IN|MH|android|mobile"
assert_eq "302" "$CAPTURED_STATUS" "a resolved code answers 302, never 301"
assert_eq "public, max-age=300" "$(captured_header cache-control)" "a resolved code caches for five minutes"

assert_eq "$PLAY_URL" "$(resolved 'IN|MH|android|mobile')" \
    "first-match precedence: the android rule wins over the later mobile rule"
assert_eq "$PLAY_URL" "$(resolved 'PK|SD|android|desktop')" \
    "OR within a dimension: the second listed country matches"
assert_eq "$MOBILE_URL" "$(resolved 'IN|MH|ios|mobile')" \
    "AND across dimensions: a matching country with a non-matching platform falls through"
assert_eq "$MOBILE_URL" "$(resolved 'DE|BE|other|mobile')" \
    "wildcard dimensions: the device-only rule ignores country, region and platform"
assert_eq "$TABLET_URL" "$(resolved 'US|CA|ios|tablet')" \
    "tablet beats mobile: a tablet is not captured by the mobile rule"
assert_eq "$DEFAULT_URL" "$(resolved 'US|NY|ios|tablet')" \
    "AND across dimensions: the right device in the wrong region falls through"
assert_eq "$DEFAULT_URL" "$(resolved 'XX|XX|other|other')" \
    "an unmatched segment falls to the link default URL"
assert_eq "$PLAY_URL" "$(resolved 'IN%7CMH%7Candroid%7Cmobile')" \
    "a percent-encoded separator resolves exactly like a literal one"

finish
