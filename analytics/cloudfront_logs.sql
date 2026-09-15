-- Source of truth for the Athena/Glue objects the rollup Lambda queries.
--
-- The table is really created by Terraform, in
-- aws-cloud/terraform/modules/analytics/main.tf (aws_glue_catalog_table.cloudfront_logs).
-- This file reproduces it verbatim so a human can run the same query in the Athena
-- console when a rollup day looks wrong. If the two ever disagree, Terraform wins and
-- this file is the bug.
--
-- Substitutions, all fixed by the aws-cloud module (prefix = "aws-cloud"):
--   <ACCOUNT_ID>  the AWS account id, also the LOGS_BUCKET suffix and the log key prefix
--   database      aws_cloud                  (GLUE_DATABASE)
--   table         cloudfront_logs            (GLUE_TABLE)
--   workgroup     aws-cloud-analytics        (ATHENA_WORKGROUP, 1 GB per-query scan cap)
--   bucket        aws-cloud-logs-<ACCOUNT_ID> (LOGS_BUCKET)
--
-- `date` and `time` are Athena reserved words: they are backticked everywhere, both in
-- the DDL and in any ad-hoc query. The rollup query never selects them — the partition
-- columns carry the day.

CREATE EXTERNAL TABLE IF NOT EXISTS aws_cloud.cloudfront_logs (
    `date`                       date,
    `time`                       string,
    x_edge_location              string,
    sc_bytes                     bigint,
    c_ip                         string,
    cs_method                    string,
    cs_host                      string,
    cs_uri_stem                  string,
    sc_status                    int,
    cs_referer                   string,
    cs_user_agent                string,
    cs_uri_query                 string,
    cs_cookie                    string,
    x_edge_result_type           string,
    x_edge_request_id            string,
    x_host_header                string,
    cs_protocol                  string,
    cs_bytes                     bigint,
    time_taken                   float,
    x_forwarded_for              string,
    ssl_protocol                 string,
    ssl_cipher                   string,
    x_edge_response_result_type  string,
    cs_protocol_version          string,
    fle_status                   string,
    fle_encrypted_fields         int,
    c_port                       int,
    time_to_first_byte           float,
    x_edge_detailed_result_type  string,
    sc_content_type              string,
    sc_content_len               bigint,
    sc_range_start               bigint,
    sc_range_end                 bigint
)
PARTITIONED BY (
    year   string,
    month  string,
    day    string
)
ROW FORMAT SERDE 'org.apache.hadoop.hive.serde2.lazy.LazySimpleSerDe'
WITH SERDEPROPERTIES (
    'field.delim'           = '\t',
    'serialization.format'  = '\t'
)
STORED AS
    INPUTFORMAT  'org.apache.hadoop.mapred.TextInputFormat'
    OUTPUTFORMAT 'org.apache.hadoop.hive.ql.io.HiveIgnoreKeyTextOutputFormat'
LOCATION 's3://aws-cloud-logs-<ACCOUNT_ID>/AWSLogs/<ACCOUNT_ID>/CloudFront/'
TBLPROPERTIES (
    'EXTERNAL'                  = 'TRUE',
    'has_encrypted_data'        = 'false',
    'skip.header.line.count'    = '2',
    'projection.enabled'        = 'true',
    'projection.year.type'      = 'integer',
    'projection.year.range'     = '2025,2035',
    'projection.year.digits'    = '4',
    'projection.month.type'     = 'integer',
    'projection.month.range'    = '1,12',
    'projection.month.digits'   = '2',
    'projection.day.type'       = 'integer',
    'projection.day.range'      = '1,31',
    'projection.day.digits'     = '2',
    'storage.location.template' = 's3://aws-cloud-logs-<ACCOUNT_ID>/AWSLogs/<ACCOUNT_ID>/CloudFront/${year}/${month}/${day}'
);


-- The nightly rollup query, exactly as lambdas/rollup/src/athena.rs builds it.
--
-- Parameters, all zero-padded strings because the projection declares digits 4/2/2:
--   :year  '2026'   :month '01'   :day '02'
--
-- The three partition predicates are what keeps the scan to one day's objects — a few MB,
-- far under the acceptance bar of 100 MB and the workgroup's 1 GB per-query cap. Drop any
-- one of them and the query reads every projected day from 2025 to 2035.
--
-- sc_status = 302 is the only status the redirect path emits on a hit, so it filters out
-- 404s, the frontend at `/`, and /api/* traffic without needing a cs_uri_stem predicate.
--
-- The GROUP BY is what the Lambda relies on: one output row per (code, segment) pair, so
-- result paging stays proportional to links x segments, not to click volume.
--
-- cs_uri_query arrives either as `s=IN|MH|android|mobile` or, when CloudFront percent-encodes
-- the separator on its way into the log, as `s=IN%7CMH%7Candroid%7Cmobile`. Both forms are one
-- segment; the Lambda decodes before it splits, so the same segment never lands in two buckets.
-- Expect both spellings side by side in the raw result set.

SELECT cs_uri_stem, cs_uri_query, count(*) AS clicks
FROM "aws_cloud"."cloudfront_logs"
WHERE year = :year
  AND month = :month
  AND day = :day
  AND sc_status = 302
GROUP BY 1, 2;
