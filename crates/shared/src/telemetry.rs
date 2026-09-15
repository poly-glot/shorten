use std::sync::LazyLock;

use chrono::Utc;
use serde_json::{Map, Value, json};
use tracing_subscriber::filter::LevelFilter;

const DEFAULT_NAMESPACE: &str = "shorten";
const NAMESPACE_ENV: &str = "METRIC_NAMESPACE";

static NAMESPACE: LazyLock<String> = LazyLock::new(|| std::env::var(NAMESPACE_ENV).unwrap_or_else(|_| DEFAULT_NAMESPACE.into()));

pub fn init_logging() {
    tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_target(false)
        .without_time()
        .with_max_level(LevelFilter::INFO)
        .init();
}

fn emf_line(namespace: &str, timestamp_millis: i64, values: &[(&str, f64)], dimensions: &[(&str, &str)], properties: &[(&str, &str)]) -> Value {
    let mut line = Map::new();
    line.insert(
        "_aws".into(),
        json!({
            "Timestamp": timestamp_millis,
            "CloudWatchMetrics": [{
                "Namespace": namespace,
                "Dimensions": [dimensions.iter().map(|(name, _)| *name).collect::<Vec<_>>()],
                "Metrics": values.iter().map(|(name, _)| json!({ "Name": name, "Unit": "None" })).collect::<Vec<_>>(),
            }],
        }),
    );

    for (name, value) in dimensions.iter().chain(properties) {
        line.insert((*name).into(), json!(value));
    }

    for (name, value) in values {
        line.insert((*name).into(), json!(value));
    }

    Value::Object(line)
}

pub fn emit(values: &[(&str, f64)], dimensions: &[(&str, &str)], properties: &[(&str, &str)]) {
    println!("{}", emf_line(&NAMESPACE, Utc::now().timestamp_millis(), values, dimensions, properties));
}

pub fn count(name: &str, dimensions: &[(&str, &str)], properties: &[(&str, &str)]) {
    emit(&[(name, 1.0)], dimensions, properties);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emf_line_prints_the_frozen_cloudwatch_embedded_metric_envelope() {
        let cases = [
            (
                "dimensions and properties are carried as top-level fields alongside the envelope",
                "shorten-test",
                1_700_000_000_000_i64,
                &[("RollupDays", 1.0)][..],
                &[("Outcome", "Written")][..],
                &[("code", "aB3xK9mQ2p")][..],
                json!({
                    "_aws": {
                        "Timestamp": 1_700_000_000_000_i64,
                        "CloudWatchMetrics": [{
                            "Namespace": "shorten-test",
                            "Dimensions": [["Outcome"]],
                            "Metrics": [{ "Name": "RollupDays", "Unit": "None" }],
                        }],
                    },
                    "Outcome": "Written",
                    "code": "aB3xK9mQ2p",
                    "RollupDays": 1.0,
                }),
            ),
            (
                "no dimensions still lists an empty dimension set, and a zero-valued metric is carried through",
                "shorten-test",
                0_i64,
                &[("LinksWritten", 3.0), ("LinksFailed", 0.0)][..],
                &[][..],
                &[("day", "2026-01-02")][..],
                json!({
                    "_aws": {
                        "Timestamp": 0_i64,
                        "CloudWatchMetrics": [{
                            "Namespace": "shorten-test",
                            "Dimensions": [[]],
                            "Metrics": [
                                { "Name": "LinksWritten", "Unit": "None" },
                                { "Name": "LinksFailed", "Unit": "None" },
                            ],
                        }],
                    },
                    "day": "2026-01-02",
                    "LinksWritten": 3.0,
                    "LinksFailed": 0.0,
                }),
            ),
            (
                "a large count is carried through at full precision",
                "shorten-test",
                0_i64,
                &[("RowsScanned", 12_345_678.0)][..],
                &[][..],
                &[][..],
                json!({
                    "_aws": {
                        "Timestamp": 0_i64,
                        "CloudWatchMetrics": [{
                            "Namespace": "shorten-test",
                            "Dimensions": [[]],
                            "Metrics": [{ "Name": "RowsScanned", "Unit": "None" }],
                        }],
                    },
                    "RowsScanned": 12_345_678.0,
                }),
            ),
        ];

        for (label, namespace, timestamp_millis, values, dimensions, properties, expected) in cases {
            let actual = emf_line(namespace, timestamp_millis, values, dimensions, properties);
            let printed = actual.to_string();
            let Ok(reparsed) = serde_json::from_str::<Value>(&printed) else {
                panic!("{label}: emf_line printed invalid JSON: {printed}");
            };

            assert_eq!(reparsed, expected, "{label}");
        }
    }
}
