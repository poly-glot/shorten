import assert from 'node:assert/strict';
import test from 'node:test';

import { codeList, isoDay } from './format.js';

const CODE_LIST_CASES = [
    { expected: ['IN', 'GB'], label: 'upper-cases and trims around the commas', value: ' in , gb ' },
    { expected: ['IN'], label: 'drops the empty field a trailing comma leaves', value: 'in,' },
    { expected: [], label: 'reads an empty input as no constraint', value: '' },
    { expected: [], label: 'reads a lone comma as no constraint', value: ' , ' },
];

for (const { expected, label, value } of CODE_LIST_CASES) {
    test(`code list ${label}`, () => {
        assert.deepEqual(codeList(value), expected, label);
    });
}

test('formats a day as its UTC calendar date', () => {
    assert.equal(isoDay(new Date('2026-09-14T23:30:00Z')), '2026-09-14');
});
