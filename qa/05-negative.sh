#!/usr/bin/env bash
set -euo pipefail

. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

USAGE="usage: qa/05-negative.sh --verify <origin-secret> [--mgmt-url <url>] [--redirect-url <url>]

Asserts every refusal path: a wrong management secret, an unknown code, a malformed code, a
rejected target URL, a rule list over the ceiling, and an origin request with no X-Origin-Verify.

  --verify        the value of SHORTEN_ORIGIN_VERIFY, sent as X-Origin-Verify
  --mgmt-url      shorten-mgmt function URL; read from AWS when omitted
  --redirect-url  shorten-redirect function URL; read from AWS when omitted"

UNUSED_CODE="AAAAAAAAAA"
MALFORMED_CODE="short"

CODE=""
SECRET=""

cleanup() {
    [ -n "$CODE" ] || return 0

    curl -gsS -o /dev/null -X DELETE \
        -H "x-origin-verify: $VERIFY" \
        -H "x-manage-secret: $SECRET" \
        "$MGMT_URL/links/$CODE" || true
}

create_status() {
    curl -gsS -o /dev/null -w '%{http_code}' -X POST "$MGMT_URL/links" \
        -H "x-origin-verify: $VERIFY" \
        -H 'content-type: application/json' \
        -d @-
}

parse_args "$@"
require_args VERIFY
require_command python3
discover_origins

created=$(curl -gsS -X POST "$MGMT_URL/links" \
    -H "x-origin-verify: $VERIFY" \
    -H 'content-type: application/json' \
    -d '{"url": "https://example.com/negative-tests"}')

CODE=$(printf '%s' "$created" | json_field code)
SECRET=$(printf '%s' "$created" | json_field manage_secret)
trap cleanup EXIT

note "code $CODE"

assert_eq "200" "$(status_of -H "x-origin-verify: $VERIFY" -H "x-manage-secret: $SECRET" "$MGMT_URL/links/$CODE")" \
    "the real secret reads the link, so the refusals below are not refusing everything"

assert_eq "401" "$(status_of -H "x-origin-verify: $VERIFY" -H "x-manage-secret: not-the-secret" "$MGMT_URL/links/$CODE")" \
    "a wrong management secret is refused with 401"

assert_eq "401" "$(status_of -H "x-origin-verify: $VERIFY" "$MGMT_URL/links/$CODE")" \
    "an absent management secret is refused with 401"

assert_eq "404" "$(status_of -H "x-origin-verify: $VERIFY" -H "x-manage-secret: $SECRET" "$MGMT_URL/links/$UNUSED_CODE")" \
    "a well-formed code that no link uses is 404 on the management API"

assert_eq "404" "$(status_of -H "x-origin-verify: $VERIFY" -H "x-manage-secret: $SECRET" "$MGMT_URL/links/$MALFORMED_CODE")" \
    "a code of the wrong length is 404 on the management API"

assert_eq "404" "$(status_of -H "x-origin-verify: $VERIFY" "$REDIRECT_URL/$UNUSED_CODE")" \
    "a well-formed code that no link uses is 404 on the redirect path"

assert_eq "404" "$(status_of -H "x-origin-verify: $VERIFY" "$REDIRECT_URL/$MALFORMED_CODE")" \
    "a malformed code is 404 on the redirect path, refused before any table read"

assert_eq "422" "$(printf '{"url": "javascript:alert(1)"}' | create_status)" \
    "a javascript: target is refused with 422"

assert_eq "422" "$(printf '{"url": "http://169.254.169.254/latest/meta-data/"}' | create_status)" \
    "a link-local IP literal target is refused with 422"

over_ceiling_body=$(python3 -c '
import json

rules = [{"countries": ["GB"], "url": "https://example.com/rule-%d" % position} for position in range(21)]
print(json.dumps({"url": "https://example.com/too-many-rules", "rules": rules}))
')

assert_eq "422" "$(printf '%s' "$over_ceiling_body" | create_status)" \
    "a twenty-first rule is refused with 422"

assert_eq "403" "$(status_of "$REDIRECT_URL/$CODE")" \
    "the redirect origin refuses a request with no X-Origin-Verify with 403"

assert_eq "403" "$(status_of -H "x-origin-verify: wrong-secret" "$REDIRECT_URL/$CODE")" \
    "the redirect origin refuses a wrong X-Origin-Verify with 403"

assert_eq "404" "$(status_of -H "x-manage-secret: $SECRET" "$MGMT_URL/links/$CODE")" \
    "the management origin answers a request with no X-Origin-Verify with 404, not 403"

assert_eq "404" "$(status_of -H "x-origin-verify: wrong-secret" -H "x-manage-secret: $SECRET" "$MGMT_URL/links/$CODE")" \
    "the management origin answers a wrong X-Origin-Verify with 404, not 403"

finish
