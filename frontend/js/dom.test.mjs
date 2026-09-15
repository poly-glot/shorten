import assert from 'node:assert/strict';
import test from 'node:test';

import { escapeHTML } from './dom.js';

const ESCAPE_CASES = [
    { expected: '&lt;script&gt;alert(1)&lt;/script&gt;', label: 'a script tag', value: '<script>alert(1)</script>' },
    { expected: '&quot;&#39;', label: 'both quote characters', value: '"\'' },
    { expected: '&amp;lt;', label: 'an already-escaped entity escaped again', value: '&lt;' },
    { expected: '7', label: 'a number arriving where a string was expected', value: 7 },
];

for (const { expected, label, value } of ESCAPE_CASES) {
    test(`escapes ${label}`, () => {
        assert.equal(escapeHTML(value), expected, label);
    });
}
