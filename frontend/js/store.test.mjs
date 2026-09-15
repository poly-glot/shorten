import assert from 'node:assert/strict';
import test from 'node:test';

import { forgetLink, rememberLink, savedLinks } from './store.js';

let stored = null;

globalThis.localStorage = {
    getItem: () => stored,
    setItem: (_key, value) => {
        stored = value;
    },
};

const older = { code: 'aaaaaaaaaa', secret: 'older-secret', url: 'https://older.example/' };
const newer = { code: 'bbbbbbbbbb', secret: 'newer-secret', url: 'https://newer.example/' };

const UNREADABLE_CASES = [
    { label: 'nothing stored', value: null },
    { label: 'text that is not JSON', value: 'not json' },
    { label: 'JSON that is not a list', value: '{"code":"aaaaaaaaaa"}' },
];

test.beforeEach(() => {
    stored = null;
});

for (const { label, value } of UNREADABLE_CASES) {
    test(`reads ${label} as no links`, () => {
        stored = value;

        assert.deepEqual(savedLinks(), [], label);
    });
}

test('puts the most recently remembered link first', () => {
    rememberLink(older);
    rememberLink(newer);

    assert.deepEqual(savedLinks(), [newer, older]);
});

test('replaces a remembered code instead of listing it twice', () => {
    const moved = { ...older, url: 'https://moved.example/' };

    rememberLink(older);
    rememberLink(moved);

    assert.deepEqual(savedLinks(), [moved]);
});

test('forgets only the named code', () => {
    rememberLink(older);
    rememberLink(newer);
    forgetLink(older.code);

    assert.deepEqual(savedLinks(), [newer]);
});

test('reads no links when storage is unreachable', () => {
    const refuse = () => {
        throw new Error('storage disabled');
    };

    globalThis.localStorage = { getItem: refuse, setItem: refuse };

    assert.deepEqual(savedLinks(), []);
});
