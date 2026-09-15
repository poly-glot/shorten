#!/usr/bin/env bash
set -euo pipefail

ENDPOINT="$AWS_ENDPOINT_URL_DYNAMODB"
TABLE="$TABLE_NAME"

if ! curl -s -o /dev/null "$ENDPOINT"; then
    case "$ENDPOINT" in
        http://localhost:*|http://127.0.0.1:*)
            command -v docker >/dev/null || { echo "DynamoDB Local is not answering at $ENDPOINT and docker is not installed to start it" >&2; exit 1; }
            echo "starting DynamoDB Local in Docker (container shorten-ddb, stop it with: docker rm -f shorten-ddb)"
            docker rm -f shorten-ddb >/dev/null 2>&1 || true
            docker run -d --rm --name shorten-ddb -p "${ENDPOINT##*:}:8000" amazon/dynamodb-local -jar DynamoDBLocal.jar -inMemory -sharedDb >/dev/null
            for _ in $(seq 1 20); do
                curl -s -o /dev/null "$ENDPOINT" && break
                sleep 1
            done
            ;;
        *)
            echo "DynamoDB Local is not answering at $ENDPOINT" >&2
            exit 1
            ;;
    esac
fi

if aws dynamodb describe-table --endpoint-url "$ENDPOINT" --table-name "$TABLE" >/dev/null 2>&1; then
    echo "table $TABLE exists"
    exit 0
fi

aws dynamodb create-table --endpoint-url "$ENDPOINT" --table-name "$TABLE" --billing-mode PAY_PER_REQUEST \
    --attribute-definitions AttributeName=PK,AttributeType=S AttributeName=SK,AttributeType=S \
    --key-schema AttributeName=PK,KeyType=HASH AttributeName=SK,KeyType=RANGE \
    >/dev/null
echo "table $TABLE created"
