#!/usr/bin/env bash
set -euo pipefail

LAMBDA="${LAMBDA_URL:-http://localhost:9000}"
MGMT="$LAMBDA/lambda-url/mgmt"
REDIRECT="$LAMBDA/lambda-url/redirect"

ANDROID_SEGMENT='IN|MH|android|mobile'
IOS_SEGMENT='US|CA|ios|tablet'

created=$(curl -sS -X POST "$MGMT/links" -H 'content-type: application/json' -d '{
    "url": "https://example.com/download",
    "rules": [
        {
            "countries": ["IN", "PK"],
            "platforms": ["android"],
            "url": "https://play.google.com/store/apps/details?id=com.example.app"
        },
        {
            "platforms": ["ios"],
            "url": "https://apps.apple.com/app/id123456789"
        }
    ]
}')

read -r code secret < <(printf '%s' "$created" | python3 -c '
import json, sys

payload = json.load(sys.stdin)
if "code" not in payload:
    sys.exit("seed failed: " + json.dumps(payload))

print(payload["code"], payload["manage_secret"])
')

resolved() {
    curl -sS -G -o /dev/null -w '%{redirect_url}' --data-urlencode "s=$1" "$REDIRECT/$code"
}

echo "seeded a demo link with country and platform rules"
echo "  code:   $code"
echo "  secret: $secret"
echo "  $ANDROID_SEGMENT -> $(resolved "$ANDROID_SEGMENT")"
echo "  $IOS_SEGMENT -> $(resolved "$IOS_SEGMENT")"
echo "paste the code and the secret into the manage panel at http://localhost:3000"
