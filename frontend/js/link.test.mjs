import assert from 'node:assert/strict';
import test from 'node:test';

import { isCode, isHttpUrl, linkProblem, rulePayload } from './link.js';

const ruleOf = (rule) => ({ countries: [], devices: [], platforms: [], regions: [], url: 'https://example.com', ...rule });
const listOf = (length) => Array.from({ length }, (unused, index) => `C${index}`);
const rulesOf = (length) => Array.from({ length }, () => ruleOf({}));

const URL_CASES = [
    { expected: true, label: 'an https URL', value: 'https://example.com/a' },
    { expected: true, label: 'an http URL', value: 'http://example.com' },
    { expected: false, label: 'a javascript: URL', value: 'javascript:alert(1)' },
    { expected: false, label: 'a data: URL', value: 'data:text/html,<script>alert(1)</script>' },
    { expected: false, label: 'a scheme-relative URL', value: '//example.com' },
    { expected: false, label: 'an empty field', value: '' },
];

for (const { expected, label, value } of URL_CASES) {
    test(`${expected ? 'accepts' : 'refuses'} ${label}`, () => {
        assert.equal(isHttpUrl(value), expected, label);
    });
}

const CODE_CASES = [
    { expected: true, label: 'ten characters from the alphabet', value: 'aB3xK9mQ2p' },
    { expected: true, label: 'the hyphen and underscore', value: 'a-b_c1234X' },
    { expected: false, label: 'nine characters', value: 'aB3xK9mQ2' },
    { expected: false, label: 'eleven characters', value: 'aB3xK9mQ2pZ' },
    { expected: false, label: 'a character outside the alphabet', value: 'aB3xK9mQ2.' },
];

for (const { expected, label, value } of CODE_CASES) {
    test(`${expected ? 'accepts' : 'refuses'} a code of ${label}`, () => {
        assert.equal(isCode(value), expected, label);
    });
}

test('omits every empty dimension from the payload so absent and empty stay distinct', () => {
    assert.deepEqual(rulePayload(ruleOf({ countries: ['IN'] })), { countries: ['IN'], url: 'https://example.com' });
});

test('carries every populated dimension into the payload', () => {
    const rule = ruleOf({ countries: ['IN'], devices: ['mobile'], platforms: ['android'], regions: ['MH'] });

    assert.deepEqual(rulePayload(rule), { countries: ['IN'], devices: ['mobile'], platforms: ['android'], regions: ['MH'], url: 'https://example.com' });
});

const RULE_PROBLEM_CASES = [
    { label: 'a rule with an http target', problem: '', rule: {} },
    { label: 'a rule with no target yet', problem: 'Rule 2 needs an http:// or https:// target URL.', rule: { url: '' } },
    { label: 'a rule whose target is a javascript: URL', problem: 'Rule 2 needs an http:// or https:// target URL.', rule: { url: 'javascript:alert(1)' } },
    { label: 'a rule at the country ceiling', problem: '', rule: { countries: listOf(30) } },
    { label: 'a rule one country over the ceiling', problem: 'Rule 2: countries takes at most 30 values.', rule: { countries: listOf(31) } },
    { label: 'a rule one region over the ceiling', problem: 'Rule 2: regions takes at most 30 values.', rule: { regions: listOf(31) } },
];

for (const { label, problem, rule } of RULE_PROBLEM_CASES) {
    test(`reports ${label} by its position`, () => {
        assert.equal(linkProblem('https://example.com', [ruleOf({}), ruleOf(rule)]), problem, label);
    });
}

test('refuses the default URL before looking at any rule', () => {
    assert.equal(linkProblem('javascript:alert(1)', [ruleOf({ url: '' })]), 'The default target must be an http:// or https:// URL.');
});

test('refuses one rule over the link ceiling', () => {
    assert.equal(linkProblem('https://example.com', rulesOf(21)), 'A link takes at most 20 rules.');
});

test('accepts a link at the rule ceiling', () => {
    assert.equal(linkProblem('https://example.com', rulesOf(20)), '');
});
