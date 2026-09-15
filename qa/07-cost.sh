#!/usr/bin/env bash
set -euo pipefail

. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

USAGE="usage: qa/07-cost.sh [--account <id>] [--table aws-cloud] [--workgroup aws-cloud-analytics]

Executes the cost checklist against the deployed account. Every item is an AWS call with an
assertion, not a printed reminder. The written report is qa/COST.md.

  --account    the AWS account id; read from STS when omitted
  --table      the shared DynamoDB table, default aws-cloud
  --workgroup  the Athena workgroup, default aws-cloud-analytics"

BUDGET_LIMIT="5"
BUDGET_NAME="aws-cloud-monthly"
FREE_CAPACITY_UNITS=25
LOG_RETENTION_DAYS=90
SCAN_CUTOFF_BYTES=1073741824
SHORTEN_FUNCTIONS="shorten-mgmt shorten-redirect shorten-rollup"

parse_args "$@"
require_command python3
discover_account

LOGS_BUCKET="aws-cloud-logs-$ACCOUNT"

billing=$(aws dynamodb describe-table --table-name "$TABLE" \
    --query 'Table.BillingModeSummary.BillingMode || `PROVISIONED`' --output text)
assert_eq "PROVISIONED" "$billing" "the shared table is provisioned, not on-demand"

read_units=$(aws dynamodb describe-table --table-name "$TABLE" \
    --query 'sum([Table.ProvisionedThroughput.ReadCapacityUnits, sum(Table.GlobalSecondaryIndexes[].ProvisionedThroughput.ReadCapacityUnits)])' \
    --output text)
write_units=$(aws dynamodb describe-table --table-name "$TABLE" \
    --query 'sum([Table.ProvisionedThroughput.WriteCapacityUnits, sum(Table.GlobalSecondaryIndexes[].ProvisionedThroughput.WriteCapacityUnits)])' \
    --output text)

assert_eq "$FREE_CAPACITY_UNITS" "$read_units" "read capacity across the table and its indexes is the free $FREE_CAPACITY_UNITS units"
assert_eq "$FREE_CAPACITY_UNITS" "$write_units" "write capacity across the table and its indexes is the free $FREE_CAPACITY_UNITS units"

scalable=$(aws application-autoscaling describe-scalable-targets --service-namespace dynamodb \
    --query 'length(ScalableTargets)' --output text)
assert_eq "0" "$scalable" "no DynamoDB autoscaling target exists, so no target-tracking alarm bills"

for function_name in $SHORTEN_FUNCTIONS; do
    table_in_use=$(aws lambda get-function-configuration --function-name "$function_name" \
        --query 'Environment.Variables.TABLE_NAME' --output text)
    assert_eq "$TABLE" "$table_in_use" "$function_name writes to the shared table, not to one of its own"

    vpc=$(aws lambda get-function-configuration --function-name "$function_name" \
        --query 'VpcConfig.VpcId || `none`' --output text)
    assert_eq "none" "$vpc" "$function_name runs outside a VPC"
done

nat_gateways=$(aws ec2 describe-nat-gateways \
    --filter 'Name=state,Values=pending,available' --query 'length(NatGateways)' --output text)
assert_eq "0" "$nat_gateways" "the account runs no NAT gateway"

lifecycle_days=$(aws s3api get-bucket-lifecycle-configuration --bucket "$LOGS_BUCKET" \
    --query "Rules[?Status=='Enabled'].Expiration.Days | [0]" --output text)
assert_eq "$LOG_RETENTION_DAYS" "$lifecycle_days" "the logs bucket expires objects after $LOG_RETENTION_DAYS days"

cutoff=$(aws athena get-work-group --work-group "$ATHENA_WORKGROUP" \
    --query 'WorkGroup.Configuration.BytesScannedCutoffPerQuery' --output text)
assert_eq "$SCAN_CUTOFF_BYTES" "$cutoff" "the Athena workgroup caps a query at 1 GB scanned"

enforced=$(aws athena get-work-group --work-group "$ATHENA_WORKGROUP" \
    --query 'WorkGroup.Configuration.EnforceWorkGroupConfiguration' --output text)
assert_eq "True" "$enforced" "the workgroup enforces its configuration, so a client cannot opt out of the cap"

budget_limit=$(aws budgets describe-budget --account-id "$ACCOUNT" --budget-name "$BUDGET_NAME" \
    --query 'Budget.BudgetLimit.Amount' --output text 2>/dev/null || printf 'missing')
assert_eq "$BUDGET_LIMIT" "${budget_limit%%.*}" "the \$$BUDGET_LIMIT monthly budget alarm exists"

notifications=$(aws budgets describe-notifications-for-budget --account-id "$ACCOUNT" --budget-name "$BUDGET_NAME" \
    --query 'length(Notifications)' --output text 2>/dev/null || printf '0')
assert_eq "yes" "$(python3 -c "print('yes' if int('$notifications') > 0 else 'no')")" \
    "the budget notifies somebody rather than sitting silent"

alarm_count=$(aws cloudwatch describe-alarms --alarm-name-prefix shorten- \
    --query 'length(MetricAlarms)' --output text)
assert_eq "3" "$alarm_count" "shorten owns exactly three CloudWatch alarms, the budgeted number"

finish
