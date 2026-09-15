use std::ops::RangeInclusive;

use crate::error::AppError;

pub const MAX_URL_LEN: usize = 2048;

const ALLOWED_SCHEMES: [&str; 2] = ["http://", "https://"];
const AUTHORITY_END: [char; 3] = ['/', '?', '#'];
const LABEL_LENS: RangeInclusive<usize> = 1..=63;
const MIN_SUFFIX_LEN: usize = 2;
const PRIVATE_SUFFIXES: [&str; 7] = ["corp", "home", "internal", "intranet", "lan", "local", "localhost"];

fn refused(reason: &str) -> AppError {
    AppError::BadRequest(format!("url rejected: {reason}"))
}

fn after_scheme(raw: &str) -> Option<&str> {
    ALLOWED_SCHEMES
        .iter()
        .find(|scheme| raw.get(..scheme.len()).is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme)))
        .map(|scheme| &raw[scheme.len()..])
}

fn host_of(authority: &str) -> Result<&str, AppError> {
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, host_port)| host_port);
    if host_port.starts_with('[') {
        return Err(refused("ip literal hosts are not allowed"));
    }

    let Some((host, port)) = host_port.split_once(':') else {
        return Ok(host_port);
    };
    if port.contains(':') {
        return Err(refused("host has more than one port separator"));
    }
    if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(refused("port is not a number"));
    }

    Ok(host)
}

fn is_dns_label(label: &str) -> bool {
    LABEL_LENS.contains(&label.len())
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn is_public_suffix(suffix: &str) -> bool {
    suffix.len() >= MIN_SUFFIX_LEN && suffix.bytes().all(|byte| byte.is_ascii_alphabetic())
}

fn check_host(host: &str) -> Result<(), AppError> {
    if host.is_empty() {
        return Err(refused("no host"));
    }
    if host.ends_with('.') {
        return Err(refused("host has a trailing dot"));
    }

    let labels: Vec<&str> = host.split('.').collect();
    if labels.iter().any(|label| !is_dns_label(label)) {
        return Err(refused("host is not a domain name"));
    }

    let [_, .., suffix] = labels[..] else {
        return Err(refused("host is not a public domain name"));
    };
    if !is_public_suffix(suffix) {
        return Err(refused("host is an ip literal or has no public suffix"));
    }
    if PRIVATE_SUFFIXES.iter().any(|private| suffix.eq_ignore_ascii_case(private)) {
        return Err(refused("host is a private network name"));
    }

    Ok(())
}

pub fn validate(raw: &str) -> Result<(), AppError> {
    if raw.is_empty() {
        return Err(refused("empty"));
    }
    if raw.len() > MAX_URL_LEN {
        return Err(refused(&format!("longer than {MAX_URL_LEN} characters")));
    }
    if raw.chars().any(|character| character.is_whitespace() || character.is_control()) {
        return Err(refused("contains whitespace or control characters"));
    }

    let Some(rest) = after_scheme(raw) else {
        return Err(refused("scheme is not http or https"));
    };

    let authority = rest.split(AUTHORITY_END).next().unwrap_or_default();
    if authority.contains('\\') {
        return Err(refused("authority contains a backslash"));
    }

    check_host(host_of(authority)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUBLIC_PREFIX: &str = "https://example.com/";

    fn padded_to(length: usize) -> String {
        format!("{PUBLIC_PREFIX}{}", "a".repeat(length - PUBLIC_PREFIX.len()))
    }

    fn refusal(raw: &str) -> String {
        match validate(raw) {
            Err(AppError::BadRequest(message)) => message,
            other => panic!("{raw:?}: expected a bad request, got {other:?}"),
        }
    }

    #[test]
    fn a_public_http_url_is_accepted() {
        let cases = [
            ("a plain https url", "https://example.com/".to_string()),
            ("no trailing slash", "https://example.com".into()),
            ("a deep path with a query and a fragment", "https://sub.example.co.uk/a/b?q=1&r=2#top".into()),
            ("an explicit port", "https://example.com:8443/x".into()),
            ("a mixed-case scheme and host", "HtTpS://Example.COM/x".into()),
            ("plain http", "http://example.org/x".into()),
            ("a play store target", "https://play.google.com/store/apps/details?id=com.example.app".into()),
            ("a hyphenated label", "https://my-app.example.com/".into()),
            ("a numeric label that is not the suffix", "https://1.example.com/".into()),
            ("userinfo in front of a public host", "https://user:pw@example.com/".into()),
            ("exactly the length limit", padded_to(MAX_URL_LEN)),
        ];

        for (label, raw) in cases {
            if let Err(refused) = validate(&raw) {
                panic!("{label}: {raw} was refused: {refused}");
            }
        }
    }

    #[test]
    fn a_hostile_or_non_public_url_is_refused_with_the_reason_named() {
        let cases = [
            ("empty", String::new(), "empty"),
            ("a javascript scheme", "javascript:alert(1)".into(), "scheme is not http or https"),
            ("a data scheme", "data:text/html;base64,PHNjcmlwdD4=".into(), "scheme is not http or https"),
            ("a file scheme", "file:///etc/passwd".into(), "scheme is not http or https"),
            ("an ftp scheme", "ftp://example.com/x".into(), "scheme is not http or https"),
            ("a scheme-relative url", "//example.com/x".into(), "scheme is not http or https"),
            ("a bare host", "example.com".into(), "scheme is not http or https"),
            ("no host at all", "http://".into(), "no host"),
            ("loopback by name", "http://localhost/".into(), "host is not a public domain name"),
            (
                "loopback by name with a port",
                "http://localhost:3000/".into(),
                "host is not a public domain name",
            ),
            ("a trailing-dot evasion", "http://localhost./".into(), "host has a trailing dot"),
            ("a trailing-dot public name", "https://example.com./".into(), "host has a trailing dot"),
            ("a localhost subdomain", "http://api.localhost/".into(), "host is a private network name"),
            ("an mdns name", "http://printer.local/".into(), "host is a private network name"),
            ("a corporate intranet name", "http://wiki.internal/x".into(), "host is a private network name"),
            (
                "loopback by address",
                "http://127.0.0.1/".into(),
                "host is an ip literal or has no public suffix",
            ),
            (
                "loopback on another octet",
                "http://127.1.2.3/".into(),
                "host is an ip literal or has no public suffix",
            ),
            ("rfc1918 ten", "http://10.0.0.1/".into(), "host is an ip literal or has no public suffix"),
            (
                "rfc1918 one seven two",
                "http://172.16.0.1/".into(),
                "host is an ip literal or has no public suffix",
            ),
            (
                "rfc1918 one nine two",
                "http://192.168.1.1/".into(),
                "host is an ip literal or has no public suffix",
            ),
            (
                "link-local metadata",
                "http://169.254.169.254/latest/meta-data/".into(),
                "host is an ip literal or has no public suffix",
            ),
            (
                "the unspecified address",
                "http://0.0.0.0/".into(),
                "host is an ip literal or has no public suffix",
            ),
            (
                "carrier grade nat",
                "http://100.64.0.1/".into(),
                "host is an ip literal or has no public suffix",
            ),
            ("a decimal ip", "http://2130706433/".into(), "host is not a public domain name"),
            (
                "a hex-labelled ip",
                "http://0x7f.0.0.1/".into(),
                "host is an ip literal or has no public suffix",
            ),
            ("ipv6 loopback", "http://[::1]/".into(), "ip literal hosts are not allowed"),
            ("ipv6 unique local", "http://[fd00::1]/".into(), "ip literal hosts are not allowed"),
            ("ipv4-mapped ipv6", "http://[::ffff:127.0.0.1]/".into(), "ip literal hosts are not allowed"),
            (
                "userinfo hiding a loopback host",
                "http://example.com@127.0.0.1/".into(),
                "host is an ip literal or has no public suffix",
            ),
            (
                "userinfo hiding metadata",
                "https://www.google.com@169.254.169.254/".into(),
                "host is an ip literal or has no public suffix",
            ),
            (
                "a backslash authority evasion",
                "http://example.com\\@evil.com/".into(),
                "authority contains a backslash",
            ),
            (
                "an embedded newline",
                "http://example.com/\nHost: evil".into(),
                "contains whitespace or control characters",
            ),
            ("an embedded space", "http://exam ple.com/".into(), "contains whitespace or control characters"),
            ("an underscore label", "http://exa_mple.com/".into(), "host is not a domain name"),
            ("a leading hyphen label", "http://-example.com/".into(), "host is not a domain name"),
            ("an empty label", "http://example..com/".into(), "host is not a domain name"),
            (
                "a label over sixty-three characters",
                format!("http://{}.com/", "a".repeat(64)),
                "host is not a domain name",
            ),
            ("a non-numeric port", "http://example.com:notaport/".into(), "port is not a number"),
            ("an empty port", "http://example.com:/".into(), "port is not a number"),
            (
                "a second port separator",
                "http://example.com:80:81/".into(),
                "host has more than one port separator",
            ),
            ("one over the length limit", padded_to(MAX_URL_LEN + 1), "longer than 2048 characters"),
        ];

        for (label, raw, reason) in cases {
            assert_eq!(refusal(&raw), format!("url rejected: {reason}"), "{label}");
        }
    }
}
