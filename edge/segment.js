var SEGMENT_INCLUDE_REGION = true;

var COUNTRY_PATTERN = /^[A-Z]{2}$/;
var OTHER = 'other';
var REGION_PATTERN = /^[A-Z0-9]{1,3}$/;
var UNKNOWN = 'XX';

function headerValue(headers, name) {
    var header = headers[name];

    return header && header.value ? header.value : '';
}

function isTrue(headers, name) {
    return headerValue(headers, name) === 'true';
}

function codeOrUnknown(headers, name, pattern) {
    var code = headerValue(headers, name).toUpperCase();

    return pattern.test(code) ? code : UNKNOWN;
}

function platformOf(headers) {
    if (isTrue(headers, 'cloudfront-is-android-viewer')) return 'android';
    if (isTrue(headers, 'cloudfront-is-ios-viewer')) return 'ios';

    return OTHER;
}

function deviceOf(headers) {
    if (isTrue(headers, 'cloudfront-is-tablet-viewer')) return 'tablet';
    if (isTrue(headers, 'cloudfront-is-mobile-viewer')) return 'mobile';
    if (isTrue(headers, 'cloudfront-is-desktop-viewer')) return 'desktop';
    if (isTrue(headers, 'cloudfront-is-smarttv-viewer')) return 'tv';

    return OTHER;
}

function regionOf(headers) {
    if (!SEGMENT_INCLUDE_REGION) return UNKNOWN;

    return codeOrUnknown(headers, 'cloudfront-viewer-country-region', REGION_PATTERN);
}

function handler(event) {
    var request = event.request;
    var headers = request.headers || {};

    var segment = codeOrUnknown(headers, 'cloudfront-viewer-country', COUNTRY_PATTERN)
        + '|' + regionOf(headers)
        + '|' + platformOf(headers)
        + '|' + deviceOf(headers);

    request.querystring = { s: { value: encodeURIComponent(segment) } };

    return request;
}
