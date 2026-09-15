ACCOUNT="${ACCOUNT:-}"
ATHENA_WORKGROUP="${ATHENA_WORKGROUP:-aws-cloud-analytics}"
DOMAIN="${DOMAIN:-}"
DURATION="${DURATION:-120}"
GLUE_DATABASE="${GLUE_DATABASE:-aws_cloud}"
GLUE_TABLE="${GLUE_TABLE:-cloudfront_logs}"
MGMT_URL="${MGMT_URL:-}"
RATE="${RATE:-20}"
REDIRECT_URL="${REDIRECT_URL:-}"
TABLE="${TABLE:-aws-cloud}"
VERIFY="${VERIFY:-}"
WAIT="${WAIT:-1200}"

FAILURES=0

CAPTURED_HEADERS=""
CAPTURED_STATUS=""

usage() {
    printf '%s\n' "${USAGE:-no usage was declared}" >&2
}

die() {
    printf 'ERROR %s\n' "$*" >&2
    exit 2
}

pass() {
    printf 'ok    %s\n' "$*"
}

note() {
    printf '      %s\n' "$*"
}

fail() {
    printf 'FAIL  %s\n' "$*" >&2
    FAILURES=$((FAILURES + 1))
}

finish() {
    if [ "$FAILURES" -ne 0 ]; then
        printf '\n%s: %d assertion(s) failed\n' "$(basename "$0")" "$FAILURES" >&2
        exit 1
    fi

    printf '\n%s: every assertion passed\n' "$(basename "$0")"
}

assert_eq() {
    if [ "$1" = "$2" ]; then
        pass "$3"
        return 0
    fi

    fail "$3: expected [$1], got [$2]"
}

assert_contains() {
    case "$2" in
        *"$1"*) pass "$3" ;;
        *) fail "$3: [$2] does not contain [$1]" ;;
    esac
}

assert_not_empty() {
    if [ -n "$1" ]; then
        pass "$2"
        return 0
    fi

    fail "$2: the value was empty"
}

value_of() {
    [ $# -ge 2 ] && [ -n "$2" ] || die "$1 needs a value"
    printf '%s' "$2"
}

parse_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --account) ACCOUNT=$(value_of "$@"); shift 2 ;;
            --domain) DOMAIN=$(value_of "$@"); shift 2 ;;
            --duration) DURATION=$(value_of "$@"); shift 2 ;;
            --mgmt-url) MGMT_URL=$(value_of "$@"); shift 2 ;;
            --rate) RATE=$(value_of "$@"); shift 2 ;;
            --redirect-url) REDIRECT_URL=$(value_of "$@"); shift 2 ;;
            --table) TABLE=$(value_of "$@"); shift 2 ;;
            --verify) VERIFY=$(value_of "$@"); shift 2 ;;
            --wait) WAIT=$(value_of "$@"); shift 2 ;;
            --workgroup) ATHENA_WORKGROUP=$(value_of "$@"); shift 2 ;;
            -h|--help) usage; exit 0 ;;
            *) usage; die "unknown argument $1" ;;
        esac
    done
}

require_args() {
    for name in "$@"; do
        eval "given=\${$name:-}"
        [ -n "$given" ] || { usage; die "$name is required"; }
    done
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "$1 is required and is not on PATH"
}

discover_account() {
    require_command aws
    [ -n "$ACCOUNT" ] && return 0

    ACCOUNT=$(aws sts get-caller-identity --query Account --output text) || die "could not read the account id; pass --account"
    note "account $ACCOUNT"
}

function_url() {
    aws lambda get-function-url-config --function-name "$1" --query FunctionUrl --output text 2>/dev/null | sed 's:/*$::'
}

discover_origins() {
    require_command aws
    require_command curl

    [ -n "$MGMT_URL" ] || MGMT_URL=$(function_url shorten-mgmt)
    [ -n "$REDIRECT_URL" ] || REDIRECT_URL=$(function_url shorten-redirect)

    [ -n "$MGMT_URL" ] || die "could not read the shorten-mgmt function URL; pass --mgmt-url"
    [ -n "$REDIRECT_URL" ] || die "could not read the shorten-redirect function URL; pass --redirect-url"

    note "mgmt origin     $MGMT_URL"
    note "redirect origin $REDIRECT_URL"
}

capture() {
    CAPTURED_HEADERS=$(curl -gsS -D - -o /dev/null "$@")
    CAPTURED_STATUS=$(printf '%s' "$CAPTURED_HEADERS" | awk '/^HTTP\//{code=$2} END{print code}')
}

captured_header() {
    printf '%s' "$CAPTURED_HEADERS" | tr -d '\r' | awk -v wanted="$1" '
        BEGIN { wanted = tolower(wanted) ":" }
        tolower(substr($0, 1, length(wanted))) == wanted { line = $0; sub(/^[^:]*:[ \t]*/, "", line); value = line }
        END { print value }'
}

status_of() {
    curl -gsS -o /dev/null -w '%{http_code}' "$@"
}

location_of() {
    curl -gsS -o /dev/null -w '%{redirect_url}' "$@"
}

json_field() {
    python3 -c '
import json
import sys

payload = json.load(sys.stdin)
key = sys.argv[1]
if key not in payload:
    sys.exit("response has no " + key + ": " + json.dumps(payload))

print(payload[key])
' "$1"
}
