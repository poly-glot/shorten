use std::collections::BTreeMap;

use aws_config::BehaviorVersion;
use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::types::{AttributeDefinition, BillingMode, KeySchemaElement, KeyType, ScalarAttributeType};
use chrono::{DateTime, Duration, NaiveDate, Utc};

use crate::error::AppError;
use crate::link::{LINK_TTL_DAYS, Link, Rule};
use crate::random;
use crate::secret;
use crate::stats::{StatsDay, ttl_of};
use crate::table::{DynamoRepo, PK, SK};

const RUN_ID_BYTES: usize = 12;

pub async fn local_repo(prefix: &str) -> Option<DynamoRepo> {
    if std::env::var("AWS_ENDPOINT_URL_DYNAMODB").is_err() {
        eprintln!("skipping {prefix}: AWS_ENDPOINT_URL_DYNAMODB not set");
        return None;
    }

    let client = Client::new(&aws_config::load_defaults(BehaviorVersion::latest()).await);
    let table = format!("{prefix}-{}", run_id());
    if let Err(error) = create_table(&client, &table).await {
        panic!("could not create {table}: {error}");
    }

    Some(DynamoRepo::new(client, table))
}

pub fn link(url: &str, rules: Vec<Rule>, secret: &str, created_at: DateTime<Utc>) -> Link {
    Link {
        clicks_total: 0,
        created_at: created_at.timestamp(),
        last_rollup: None,
        rules,
        secret_hash: secret::hash(secret),
        ttl: (created_at + Duration::days(LINK_TTL_DAYS)).timestamp(),
        url: url.into(),
    }
}

pub fn rule(url: &str) -> Rule {
    Rule {
        countries: None,
        devices: None,
        platforms: None,
        regions: None,
        url: url.into(),
    }
}

pub fn stats_day(date: NaiveDate, segments: &[(&str, u64)]) -> StatsDay {
    let seg: BTreeMap<String, u64> = segments.iter().map(|&(segment, clicks)| (segment.to_string(), clicks)).collect();

    StatsDay {
        clicks: seg.values().sum(),
        date,
        seg,
        ttl: ttl_of(date),
    }
}

async fn create_table(client: &Client, table_name: &str) -> Result<(), AppError> {
    let key_schema_element = |name: &str, key_type: KeyType| KeySchemaElement::builder().attribute_name(name).key_type(key_type).build();
    let attribute_definition = |name: &str| {
        AttributeDefinition::builder()
            .attribute_name(name)
            .attribute_type(ScalarAttributeType::S)
            .build()
    };

    client
        .create_table()
        .table_name(table_name)
        .billing_mode(BillingMode::PayPerRequest)
        .key_schema(key_schema_element(PK, KeyType::Hash)?)
        .key_schema(key_schema_element(SK, KeyType::Range)?)
        .attribute_definitions(attribute_definition(PK)?)
        .attribute_definitions(attribute_definition(SK)?)
        .send()
        .await?;

    Ok(())
}

fn run_id() -> String {
    format!("run_{}", hex::encode(random::bytes::<RUN_ID_BYTES>()))
}
