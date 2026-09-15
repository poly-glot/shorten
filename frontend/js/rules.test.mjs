import assert from 'node:assert/strict';
import test from 'node:test';

import { ruleSummary } from './rules.js';

const ruleOf = (rule) => ({ countries: [], devices: [], platforms: [], regions: [], url: 'https://example.com', ...rule });

test('names every unconstrained dimension as any', () => {
    assert.equal(ruleSummary(ruleOf({ countries: ['IN'] })), 'country: IN · region: any · platform: any · device: any → https://example.com');
});

test('joins the values of one dimension with or', () => {
    assert.match(ruleSummary(ruleOf({ platforms: ['android', 'ios'] })), /platform: android or ios/);
});

test('says there is no target yet rather than trailing an empty arrow', () => {
    assert.match(ruleSummary(ruleOf({ url: '' })), /→ no target yet$/);
});
