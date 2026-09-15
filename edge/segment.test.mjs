import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const SOURCE = readFileSync(new URL('./segment.js', import.meta.url), 'utf8');
const REGION_FLAG_ON = 'var SEGMENT_INCLUDE_REGION = true;';
const REGION_FLAG_OFF = 'var SEGMENT_INCLUDE_REGION = false;';

const loadHandler = (source) => new Function(source + '; return handler;')();
const handler = loadHandler(SOURCE);

const headersOf = (viewer) => Object.fromEntries(Object.entries(viewer).map(([name, value]) => [name, { value }]));
const eventOf = (viewer, querystring) => ({ request: { headers: headersOf(viewer), querystring: querystring ?? {}, uri: '/aB3xK9mQ2p' } });
const wireOf = (run, viewer) => run(eventOf(viewer)).querystring.s.value;
const segmentOf = (run, viewer) => decodeURIComponent(wireOf(run, viewer));

const DERIVATION_CASES = [
    {
        expected: 'IN|MH|android|mobile',
        label: 'android phone in IN/MH',
        viewer: {
            'cloudfront-is-android-viewer': 'true',
            'cloudfront-is-mobile-viewer': 'true',
            'cloudfront-viewer-country': 'IN',
            'cloudfront-viewer-country-region': 'MH',
        },
    },
    {
        expected: 'US|CA|ios|tablet',
        label: 'ipad in US/CA takes tablet over mobile',
        viewer: {
            'cloudfront-is-ios-viewer': 'true',
            'cloudfront-is-mobile-viewer': 'true',
            'cloudfront-is-tablet-viewer': 'true',
            'cloudfront-viewer-country': 'US',
            'cloudfront-viewer-country-region': 'CA',
        },
    },
    {
        expected: 'DE|NW|other|desktop',
        label: 'windows desktop in DE/NW',
        viewer: {
            'cloudfront-is-desktop-viewer': 'true',
            'cloudfront-viewer-country': 'DE',
            'cloudfront-viewer-country-region': 'NW',
        },
    },
    {
        expected: 'XX|XX|other|other',
        label: 'curl sending no viewer headers at all',
        viewer: {},
    },
    {
        expected: 'XX|XX|other|desktop',
        label: 'country and region carrying separator characters',
        viewer: {
            'cloudfront-is-desktop-viewer': 'true',
            'cloudfront-viewer-country': 'IN|MH',
            'cloudfront-viewer-country-region': 'MH&s=x',
        },
    },
    {
        expected: 'GB|ENG|android|tablet',
        label: 'lower-case country and region values',
        viewer: {
            'cloudfront-is-android-viewer': 'true',
            'cloudfront-is-tablet-viewer': 'true',
            'cloudfront-viewer-country': 'gb',
            'cloudfront-viewer-country-region': 'eng',
        },
    },
];

for (const { expected, label, viewer } of DERIVATION_CASES) {
    test(`derives segment for ${label}`, () => {
        assert.equal(segmentOf(handler, viewer), expected, label);
    });
}

for (const { expected, label, viewer } of DERIVATION_CASES) {
    test(`writes the separator percent-encoded for ${label}`, () => {
        assert.equal(wireOf(handler, viewer), encodeURIComponent(expected), label);
    });
}

test('replaces an incoming querystring with the segment alone', () => {
    const incoming = { s: { value: 'spoofed' }, utm_source: { value: 'x' } };
    const viewer = { 'cloudfront-is-desktop-viewer': 'true', 'cloudfront-viewer-country': 'FR', 'cloudfront-viewer-country-region': 'IDF' };

    const produced = handler(eventOf(viewer, incoming)).querystring;

    assert.deepEqual(produced, { s: { value: 'FR%7CIDF%7Cother%7Cdesktop' } }, 'incoming parameters must not survive');
});

test('collapses the region when SEGMENT_INCLUDE_REGION is false', () => {
    const withoutRegion = loadHandler(SOURCE.replace(REGION_FLAG_ON, REGION_FLAG_OFF));
    const viewer = {
        'cloudfront-is-android-viewer': 'true',
        'cloudfront-is-mobile-viewer': 'true',
        'cloudfront-viewer-country': 'IN',
        'cloudfront-viewer-country-region': 'MH',
    };

    assert.equal(segmentOf(withoutRegion, viewer), 'IN|XX|android|mobile', 'region must fall back to XX');
});

test('keeps the build flag line the substitution depends on', () => {
    assert.ok(SOURCE.includes(REGION_FLAG_ON), 'segment.js must declare the flag verbatim on one line');
});
