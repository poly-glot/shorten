#!/usr/bin/env bash
set -euo pipefail

. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

USAGE="usage: qa/04-log-prefix.sh [--account <id>] [--workgroup <name>]

Asserts that the objects CloudFront actually delivers sit under the prefix the Glue table
projects. If they do not, partition projection matches nothing and every rollup returns zero
rows without erroring.

  --account  the AWS account id; read from STS when omitted"

parse_args "$@"
discover_account

BUCKET="aws-cloud-logs-$ACCOUNT"
PREFIX="AWSLogs/$ACCOUNT/CloudFront/"
EXPECTED_LOCATION="s3://$BUCKET/$PREFIX"
EXPECTED_TEMPLATE="s3://$BUCKET/${PREFIX}\${year}/\${month}/\${day}"

delivered=$(aws s3 ls "s3://$BUCKET/" --recursive | sed -n '1,20p' || true)

if [ -z "$delivered" ]; then
    fail "s3://$BUCKET/ is empty: no CloudFront log has been delivered yet, so the prefix cannot be confirmed"
    finish
fi

printf '%s\n' "$delivered" | sed 's/^/      /'

first_key=$(printf '%s\n' "$delivered" | head -n 1 | awk '{print $4}')
assert_not_empty "$first_key" "the logs bucket lists at least one delivered object"

under_prefix=$(printf '%s\n' "$delivered" | awk -v prefix="$PREFIX" '$4 ~ "^" prefix { found = 1 } END { print (found ? "yes" : "no") }')
assert_eq "yes" "$under_prefix" "delivered objects sit under $PREFIX, the prefix the Glue table projects"

location=$(aws glue get-table --database-name "$GLUE_DATABASE" --name "$GLUE_TABLE" \
    --query 'Table.StorageDescriptor.Location' --output text)
assert_eq "${EXPECTED_LOCATION%/}" "${location%/}" "the Glue table's location names the delivery prefix"

template=$(aws glue get-table --database-name "$GLUE_DATABASE" --name "$GLUE_TABLE" \
    --query 'Table.Parameters."storage.location.template"' --output text)
assert_eq "$EXPECTED_TEMPLATE" "$template" "partition projection templates year/month/day under the delivery prefix"

projection=$(aws glue get-table --database-name "$GLUE_DATABASE" --name "$GLUE_TABLE" \
    --query 'Table.Parameters."projection.enabled"' --output text)
assert_eq "true" "$projection" "partition projection is enabled, so no crawler runs and bills"

dated_key=$(printf '%s\n' "$delivered" | awk -v prefix="$PREFIX" '$4 ~ "^" prefix && $4 != prefix { print $4; exit }')
day_path=${dated_key#"$PREFIX"}
matches_template=$(printf '%s' "$day_path" | awk -F/ '{ print (NF >= 4 && $1 ~ /^[0-9][0-9][0-9][0-9]$/ && $2 ~ /^[0-9][0-9]$/ && $3 ~ /^[0-9][0-9]$/ ? "yes" : "no") }')
assert_eq "yes" "$matches_template" "the delivered key continues yyyy/MM/dd, matching the projected digit widths ($day_path)"

finish
