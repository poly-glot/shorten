use std::fmt;

pub const SEPARATOR: char = '|';
pub const UNKNOWN_CODE: &str = "XX";
pub const UNKNOWN_KIND: &str = "other";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment<'a> {
    pub country: &'a str,
    pub region: &'a str,
    pub platform: &'a str,
    pub device: &'a str,
}

impl<'a> Segment<'a> {
    pub const UNKNOWN: Segment<'static> = Segment {
        country: UNKNOWN_CODE,
        region: UNKNOWN_CODE,
        platform: UNKNOWN_KIND,
        device: UNKNOWN_KIND,
    };

    pub fn parse(raw: &'a str) -> Segment<'a> {
        let mut parts = raw.splitn(5, SEPARATOR);
        let fields = [parts.next(), parts.next(), parts.next(), parts.next(), parts.next()];
        let [Some(country), Some(region), Some(platform), Some(device), None] = fields else {
            return Self::UNKNOWN;
        };

        if [country, region, platform, device].iter().any(|value| value.is_empty()) {
            return Self::UNKNOWN;
        }

        Segment {
            country,
            region,
            platform,
            device,
        }
    }
}

impl fmt::Display for Segment<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}{SEPARATOR}{}{SEPARATOR}{}{SEPARATOR}{}",
            self.country, self.region, self.platform, self.device
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_canonical_string_parses_into_its_four_dimensions() {
        let cases = [
            ("an android phone in Maharashtra", "IN|MH|android|mobile", ("IN", "MH", "android", "mobile")),
            ("an iPad in California", "US|CA|ios|tablet", ("US", "CA", "ios", "tablet")),
            ("a desktop in Germany with no region", "DE|XX|other|desktop", ("DE", "XX", "other", "desktop")),
            ("a smart tv", "GB|ENG|other|tv", ("GB", "ENG", "other", "tv")),
            ("the unknown segment itself", "XX|XX|other|other", ("XX", "XX", "other", "other")),
        ];
        for (label, raw, expected) in cases {
            let segment = Segment::parse(raw);
            assert_eq!((segment.country, segment.region, segment.platform, segment.device), expected, "{label}");
        }
    }

    #[test]
    fn a_malformed_segment_reads_as_unknown_rather_than_failing() {
        let cases = [
            ("empty", ""),
            ("one dimension", "IN"),
            ("three dimensions", "IN|MH|android"),
            ("five dimensions", "IN|MH|android|mobile|extra"),
            ("an empty country", "|MH|android|mobile"),
            ("an empty device", "IN|MH|android|"),
            ("a querystring that was never a segment", "code=aB3xK9mQ2p"),
            ("separators only", "|||"),
        ];
        for (label, raw) in cases {
            assert_eq!(Segment::parse(raw), Segment::UNKNOWN, "{label}: {raw:?}");
        }
    }

    #[test]
    fn display_writes_the_canonical_string_a_parse_round_trips() {
        let cases = [
            ("a parsed segment", Segment::parse("IN|MH|android|mobile"), "IN|MH|android|mobile"),
            ("the unknown segment", Segment::UNKNOWN, "XX|XX|other|other"),
        ];
        for (label, segment, expected) in cases {
            assert_eq!(segment.to_string(), expected, "{label}");
        }
    }
}
