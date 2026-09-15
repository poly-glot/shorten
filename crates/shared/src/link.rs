use aws_sdk_dynamodb::types::AttributeValue;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_dynamo::aws_sdk_dynamodb_1::to_attribute_value;

use crate::error::AppError;
use crate::segment::Segment;
use crate::table::{DynamoRepo, METADATA_SK, condition_failed_as_false, n, partition, s};

pub const DEVICES: [&str; 5] = ["desktop", "mobile", "other", "tablet", "tv"];
pub const LINK_TTL_DAYS: i64 = 1826;
pub const MAX_RULES: usize = 20;
pub const MAX_RULE_VALUES: usize = 30;
pub const PLATFORMS: [&str; 3] = ["android", "ios", "other"];

const COUNTRY_LEN: usize = 2;
const IF_ABSENT: &str = "attribute_not_exists(PK)";
const IF_PRESENT: &str = "attribute_exists(PK)";
const REGION_LENS: std::ops::RangeInclusive<usize> = 1..=3;
const ROLLUP_ADVANCE: &str = "ADD clicks_total :clicks SET last_rollup = :date";
const ROLLUP_ONCE_PER_DAY: &str = "attribute_exists(PK) AND (attribute_not_exists(last_rollup) OR last_rollup < :date)";

pub(crate) fn link_pk(code: &str) -> String {
    partition("L", code)
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Rule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub countries: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub devices: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regions: Option<Vec<String>>,
    pub url: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Link {
    #[serde(default)]
    pub clicks_total: u64,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_rollup: Option<String>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    pub secret_hash: String,
    pub ttl: i64,
    pub url: String,
}

#[derive(Clone, Copy)]
enum Dimension {
    Countries,
    Devices,
    Platforms,
    Regions,
}

#[derive(Clone, Copy)]
struct Field {
    assignment: &'static str,
    name: (&'static str, &'static str),
    value: &'static str,
}

const RULES_FIELD: Field = Field {
    assignment: "#rules = :rules",
    name: ("#rules", "rules"),
    value: ":rules",
};
const URL_FIELD: Field = Field {
    assignment: "#url = :url",
    name: ("#url", "url"),
    value: ":url",
};

fn is_country_code(value: &str) -> bool {
    value.len() == COUNTRY_LEN && value.bytes().all(|byte| byte.is_ascii_uppercase())
}

fn is_region_code(value: &str) -> bool {
    REGION_LENS.contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

impl Dimension {
    const ALL: [Self; 4] = [Self::Countries, Self::Devices, Self::Platforms, Self::Regions];

    fn name(self) -> &'static str {
        match self {
            Self::Countries => "countries",
            Self::Devices => "devices",
            Self::Platforms => "platforms",
            Self::Regions => "regions",
        }
    }

    fn of(self, rule: &Rule) -> Option<&[String]> {
        match self {
            Self::Countries => rule.countries.as_deref(),
            Self::Devices => rule.devices.as_deref(),
            Self::Platforms => rule.platforms.as_deref(),
            Self::Regions => rule.regions.as_deref(),
        }
    }

    fn viewer<'s>(self, seg: &Segment<'s>) -> &'s str {
        match self {
            Self::Countries => seg.country,
            Self::Devices => seg.device,
            Self::Platforms => seg.platform,
            Self::Regions => seg.region,
        }
    }

    fn accepts(self, value: &str) -> bool {
        match self {
            Self::Countries => is_country_code(value),
            Self::Devices => DEVICES.contains(&value),
            Self::Platforms => PLATFORMS.contains(&value),
            Self::Regions => is_region_code(value),
        }
    }

    fn admits(self, rule: &Rule, seg: &Segment<'_>) -> bool {
        self.of(rule).is_none_or(|values| values.iter().any(|value| value == self.viewer(seg)))
    }

    fn check(self, rule: &Rule, position: usize) -> Result<(), AppError> {
        let Some(values) = self.of(rule) else {
            return Ok(());
        };

        let dimension = self.name();
        let refused = |reason: &str| AppError::BadRequest(format!("rule {position}: {dimension} {reason}"));

        if values.is_empty() {
            return Err(refused("is an empty list"));
        }
        if values.len() > MAX_RULE_VALUES {
            return Err(refused(&format!("lists more than {MAX_RULE_VALUES} values")));
        }
        if let Some(rejected) = values.iter().find(|value| !self.accepts(value)) {
            return Err(refused(&format!("contains {rejected:?}")));
        }

        Ok(())
    }
}

impl Rule {
    pub fn validate(rules: &[Rule]) -> Result<(), AppError> {
        if rules.len() > MAX_RULES {
            return Err(AppError::BadRequest(format!("a link carries at most {MAX_RULES} rules")));
        }

        rules.iter().zip(1..).try_for_each(|(rule, position)| rule.check(position))
    }

    fn check(&self, position: usize) -> Result<(), AppError> {
        crate::url::validate(&self.url)?;

        Dimension::ALL.into_iter().try_for_each(|dimension| dimension.check(self, position))
    }

    fn matches(&self, seg: &Segment<'_>) -> bool {
        Dimension::ALL.into_iter().all(|dimension| dimension.admits(self, seg))
    }
}

impl Link {
    fn is_live(&self, now: DateTime<Utc>) -> bool {
        self.ttl >= now.timestamp()
    }
}

pub fn resolve<'a>(rules: &'a [Rule], default_url: &'a str, seg: &Segment<'_>) -> &'a str {
    rules.iter().find(|rule| rule.matches(seg)).map_or(default_url, |rule| rule.url.as_str())
}

impl DynamoRepo {
    pub async fn get_link(&self, code: &str, now: DateTime<Utc>) -> Result<Option<Link>, AppError> {
        let link: Option<Link> = self.get(link_pk(code), METADATA_SK, false).await?;

        Ok(link.filter(|link| link.is_live(now)))
    }

    pub async fn put_link_if_absent(&self, code: &str, link: &Link) -> Result<bool, AppError> {
        let keys = [("PK", link_pk(code)), ("SK", METADATA_SK.into())];

        condition_failed_as_false(self.put(link, &keys, Some(IF_ABSENT)).await)
    }

    pub async fn update_link(&self, code: &str, url: Option<&str>, rules: Option<&[Rule]>) -> Result<bool, AppError> {
        let changes: Vec<(Field, AttributeValue)> = [
            rules.map(to_attribute_value).transpose()?.map(|rules| (RULES_FIELD, rules)),
            url.map(|url| (URL_FIELD, s(url))),
        ]
        .into_iter()
        .flatten()
        .collect();

        if changes.is_empty() {
            return Err(AppError::BadRequest("nothing to update".into()));
        }

        let assignments: Vec<&str> = changes.iter().map(|(field, _)| field.assignment).collect();
        let names: Vec<(&str, &str)> = changes.iter().map(|(field, _)| field.name).collect();
        let values: Vec<(&str, AttributeValue)> = changes.into_iter().map(|(field, value)| (field.value, value)).collect();
        let expression = format!("SET {}", assignments.join(", "));

        let result = self.update(link_pk(code), METADATA_SK, &expression, Some(IF_PRESENT), &names, values).await;

        condition_failed_as_false(result)
    }

    pub async fn delete_link(&self, code: &str) -> Result<bool, AppError> {
        condition_failed_as_false(self.delete(link_pk(code), METADATA_SK, Some(IF_PRESENT)).await)
    }

    pub async fn add_rollup_clicks(&self, code: &str, date: &str, day_total: u64) -> Result<bool, AppError> {
        let values = vec![(":clicks", n(day_total)), (":date", s(date))];
        let result = self
            .update(link_pk(code), METADATA_SK, ROLLUP_ADVANCE, Some(ROLLUP_ONCE_PER_DAY), &[], values)
            .await;

        condition_failed_as_false(result)
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};

    use super::*;
    use crate::testing::{link, rule};

    const DEFAULT_URL: &str = "https://example.com/default";

    fn play_store() -> Rule {
        Rule {
            countries: Some(vec!["IN".into(), "PK".into()]),
            platforms: Some(vec!["android".into()]),
            ..rule("https://play.google.com/store/apps/details?id=com.example.app")
        }
    }

    fn app_store() -> Rule {
        Rule {
            platforms: Some(vec!["ios".into()]),
            ..rule("https://apps.apple.com/app/id123456789")
        }
    }

    fn maharashtra_tablet() -> Rule {
        Rule {
            countries: Some(vec!["IN".into()]),
            devices: Some(vec!["tablet".into(), "desktop".into()]),
            regions: Some(vec!["MH".into()]),
            ..rule("https://example.com/in-mh-big-screen")
        }
    }

    fn refusal(rules: &[Rule]) -> String {
        match Rule::validate(rules) {
            Err(AppError::BadRequest(message)) => message,
            other => panic!("expected a BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn resolve_returns_the_first_rule_that_matches_every_listed_dimension() {
        let rules = [maharashtra_tablet(), play_store(), app_store()];
        let cases = [
            ("no rules at all falls through to the default", &[][..], "IN|MH|android|mobile", DEFAULT_URL),
            (
                "the first matching rule wins even though a later one also matches",
                &rules[..],
                "IN|MH|android|tablet",
                "https://example.com/in-mh-big-screen",
            ),
            (
                "an and across dimensions skips the narrower rule",
                &rules[..],
                "IN|KA|android|mobile",
                "https://play.google.com/store/apps/details?id=com.example.app",
            ),
            (
                "an or within a list matches the second value",
                &rules[..],
                "PK|PB|android|mobile",
                "https://play.google.com/store/apps/details?id=com.example.app",
            ),
            (
                "an omitted dimension is a wildcard",
                &rules[..],
                "DE|BE|ios|desktop",
                "https://apps.apple.com/app/id123456789",
            ),
            ("the unknown segment falls through to the default", &rules[..], "XX|XX|other|other", DEFAULT_URL),
            ("a malformed segment falls through to the default", &rules[..], "nonsense", DEFAULT_URL),
            (
                "a matching country with the wrong platform falls through",
                &rules[..],
                "IN|MH|other|mobile",
                DEFAULT_URL,
            ),
        ];

        for (label, rules, raw, expected) in cases {
            assert_eq!(resolve(rules, DEFAULT_URL, &Segment::parse(raw)), expected, "{label}");
        }
    }

    #[test]
    fn a_rule_with_no_dimensions_catches_every_segment() {
        let rules = [rule("https://example.com/catch-all"), play_store()];
        let cases = ["IN|MH|android|mobile", "XX|XX|other|other", "US|CA|ios|tablet"];

        for raw in cases {
            assert_eq!(resolve(&rules, DEFAULT_URL, &Segment::parse(raw)), "https://example.com/catch-all", "{raw}");
        }
    }

    #[test]
    fn validate_accepts_a_well_formed_rule_list() {
        let rules = [maharashtra_tablet(), play_store(), app_store()];

        if let Err(refused) = Rule::validate(&rules) {
            panic!("a well-formed list was refused: {refused}");
        }
    }

    #[test]
    fn validate_refuses_a_rule_list_that_breaks_a_stated_limit() {
        let wildcard = rule("https://example.com/x");
        let cases = [
            ("more than twenty rules", vec![wildcard.clone(); MAX_RULES + 1]),
            (
                "more than thirty values in a list",
                vec![Rule {
                    countries: Some(vec!["IN".to_string(); MAX_RULE_VALUES + 1]),
                    ..wildcard.clone()
                }],
            ),
            (
                "a lowercase country code",
                vec![Rule {
                    countries: Some(vec!["in".into()]),
                    ..wildcard.clone()
                }],
            ),
            (
                "a three-letter country code",
                vec![Rule {
                    countries: Some(vec!["IND".into()]),
                    ..wildcard.clone()
                }],
            ),
            (
                "a lowercase region code",
                vec![Rule {
                    regions: Some(vec!["mh".into()]),
                    ..wildcard.clone()
                }],
            ),
            (
                "a four-character region code",
                vec![Rule {
                    regions: Some(vec!["ABCD".into()]),
                    ..wildcard.clone()
                }],
            ),
            (
                "an unknown platform",
                vec![Rule {
                    platforms: Some(vec!["windows".into()]),
                    ..wildcard.clone()
                }],
            ),
            (
                "an unknown device",
                vec![Rule {
                    devices: Some(vec!["watch".into()]),
                    ..wildcard.clone()
                }],
            ),
            (
                "an empty dimension list",
                vec![Rule {
                    devices: Some(Vec::new()),
                    ..wildcard.clone()
                }],
            ),
            (
                "a rule target that is not a public http url",
                vec![rule("http://169.254.169.254/latest/meta-data/")],
            ),
        ];

        for (label, rules) in cases {
            let refused = Rule::validate(&rules);
            assert!(matches!(refused, Err(AppError::BadRequest(_))), "{label}: got {refused:?}");
        }
    }

    #[test]
    fn validate_accepts_exactly_the_stated_limits() {
        let wildcard = rule("https://example.com/x");
        let full_list = Rule {
            countries: Some(vec!["IN".to_string(); MAX_RULE_VALUES]),
            ..wildcard.clone()
        };
        let cases = [
            ("twenty rules are allowed", vec![wildcard; MAX_RULES]),
            ("thirty values are allowed", vec![full_list]),
        ];

        for (label, rules) in cases {
            if let Err(refused) = Rule::validate(&rules) {
                panic!("{label}: {refused}");
            }
        }
    }

    #[test]
    fn a_refusal_names_the_rule_by_the_position_the_page_shows() {
        let rules = [
            rule("https://example.com/first"),
            Rule {
                devices: Some(vec!["watch".into()]),
                ..rule("https://example.com/second")
            },
        ];

        assert_eq!(refusal(&rules), "rule 2: devices contains \"watch\"");
    }

    #[test]
    fn a_refusal_names_the_dimension_and_the_reason() {
        let wildcard = rule("https://example.com/x");
        let cases = [
            (
                "an empty list",
                Rule {
                    countries: Some(Vec::new()),
                    ..wildcard.clone()
                },
                "rule 1: countries is an empty list",
            ),
            (
                "an overlong list",
                Rule {
                    regions: Some(vec!["MH".to_string(); MAX_RULE_VALUES + 1]),
                    ..wildcard.clone()
                },
                "rule 1: regions lists more than 30 values",
            ),
            (
                "a rejected value",
                Rule {
                    platforms: Some(vec!["windows".into()]),
                    ..wildcard
                },
                "rule 1: platforms contains \"windows\"",
            ),
        ];

        for (label, rule, expected) in cases {
            assert_eq!(refusal(&[rule]), expected, "{label}");
        }
    }

    #[test]
    fn a_link_is_live_until_the_second_its_ttl_names() {
        let Some(expiry) = Utc.with_ymd_and_hms(2031, 1, 1, 0, 0, 0).single() else {
            panic!("a representable expiry");
        };
        let created = expiry - Duration::days(LINK_TTL_DAYS);
        let subject = link("https://example.com", Vec::new(), "secret", created);
        let cases = [
            ("the second before expiry", expiry - Duration::seconds(1), true),
            ("the second of expiry", expiry, true),
            ("the second after expiry", expiry + Duration::seconds(1), false),
        ];

        for (label, now, expected) in cases {
            assert_eq!(subject.is_live(now), expected, "{label}");
        }
    }
}
