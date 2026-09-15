import assert from 'node:assert/strict';
import test from 'node:test';

import { byDateDescending, splitSegment, statsPath, topSegments } from './stats.js';

const DAY_MS = 86400000;
const UNKNOWN_SEGMENT = { country: 'XX', device: 'other', platform: 'other', region: 'XX' };

const SEGMENT_CASES = [
    { expected: { country: 'IN', device: 'mobile', platform: 'android', region: 'MH' }, label: 'a four-field segment', segment: 'IN|MH|android|mobile' },
    { expected: UNKNOWN_SEGMENT, label: 'a segment with a field too few', segment: 'IN|MH|android' },
    { expected: UNKNOWN_SEGMENT, label: 'a segment with a field too many', segment: 'IN|MH|android|mobile|x' },
    { expected: UNKNOWN_SEGMENT, label: 'an empty segment', segment: '' },
];

for (const { expected, label, segment } of SEGMENT_CASES) {
    test(`splits ${label}`, () => {
        assert.deepEqual(splitSegment(segment), expected, label);
    });
}

test('sums a segment across every day in the window', () => {
    const days = [
        { date: '2026-09-01', seg: { 'IN|MH|android|mobile': 2 } },
        { date: '2026-09-02', seg: { 'GB|ENG|ios|tablet': 1, 'IN|MH|android|mobile': 3 } },
    ];

    const [busiest, quietest] = topSegments(days, 25);

    assert.deepEqual([busiest.clicks, quietest.clicks], [5, 1], 'the repeated segment adds up and sorts first');
});

test('reads a day carrying no segment map as no clicks', () => {
    assert.deepEqual(topSegments([{ date: '2026-09-01' }], 25), []);
});

test('keeps only the busiest segments up to the limit', () => {
    const seg = Object.fromEntries([0, 1, 2, 3, 4].map((clicks) => [`C${clicks}|XX|other|other`, clicks]));

    const top = topSegments([{ date: '2026-09-01', seg }], 2);

    assert.deepEqual(top.map(({ clicks }) => clicks), [4, 3]);
});

test('orders days newest first', () => {
    const days = [{ date: '2026-09-01' }, { date: '2026-09-03' }, { date: '2026-09-02' }];

    assert.deepEqual(byDateDescending(days).map(({ date }) => date), ['2026-09-03', '2026-09-02', '2026-09-01']);
});

test("leaves the caller's day list untouched when ordering it", () => {
    const days = [{ date: '2026-09-01' }, { date: '2026-09-03' }];

    byDateDescending(days);

    assert.deepEqual(days.map(({ date }) => date), ['2026-09-01', '2026-09-03'], 'the response array must not be sorted in place');
});

test('escapes a code before putting it in the stats path', () => {
    assert.match(statsPath('a/b?c=d'), /^\/links\/a%2Fb%3Fc%3Dd\/stats\?/);
});

test('asks for a ninety-day window inclusive of both ends', () => {
    const [, from, to] = statsPath('aB3xK9mQ2p').match(/from=(\d{4}-\d{2}-\d{2})&to=(\d{4}-\d{2}-\d{2})$/);

    const spanDays = (Date.parse(to) - Date.parse(from)) / DAY_MS + 1;

    assert.equal(spanDays, 90, 'the window must match the STATS_WINDOW_DAYS the page reports');
});
