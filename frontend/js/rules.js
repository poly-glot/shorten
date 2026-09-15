import { el, element, escapeHTML } from "./dom.js";
import { codeList } from "./format.js";
import { RULE_LIMIT } from "./link.js";

const DEVICES = ["mobile", "tablet", "desktop", "tv", "other"];
const PLATFORMS = ["android", "ios", "other"];
const RULE_LISTS = {
    create: { add: "create-add", count: "create-count", list: "create-rules" },
    edit: { add: "edit-add", count: "edit-count", list: "edit-rules" },
};

const NO_RULES_MSG = "No rules yet. Every visitor goes to the default URL.";
const NO_TARGET_MSG = "no target yet";

const ruleCountMessage = (count) => `${count} of ${RULE_LIMIT} rules. Checked top to bottom; the default URL catches everything else.`;

const dimension = (label, values) => `${label}: ${values.length ? values.join(" or ") : "any"}`;

export const ruleSummary = ({ countries, devices, platforms, regions, url }) =>
    `${dimension("country", countries)} · ${dimension("region", regions)} · ${dimension("platform", platforms)} · ${dimension("device", devices)} → ${url || NO_TARGET_MSG}`;

const checkedValues = (row, group) =>
    [...row.querySelectorAll(`input[data-group="${group}"]:checked`)].map((input) => input.value);

const fieldIn = (row, name) => row.querySelector(`[data-field="${name}"]`);

const readRule = (row) => ({
    countries: codeList(fieldIn(row, "countries").value),
    devices: checkedValues(row, "devices"),
    platforms: checkedValues(row, "platforms"),
    regions: codeList(fieldIn(row, "regions").value),
    url: fieldIn(row, "url").value.trim(),
});

export const readRules = (name) => [...el(RULE_LISTS[name].list).children].map(readRule);

function checkMarkup(group, values, selected) {
    return values
        .map((value) => {
            const checked = selected.includes(value) ? " checked" : "";

            return `<label class="check"><input data-group="${group}" type="checkbox" value="${value}"${checked}><span>${value}</span></label>`;
        })
        .join("");
}

function ruleRow({ countries = [], devices = [], platforms = [], regions = [], url = "" }) {
    const html = `
        <li class="rule">
            <div class="rule-head">
                <span class="rule-index" data-index></span>
                <span>
                    <button class="icon" data-move="up" type="button" aria-label="Move this rule up">↑</button>
                    <button class="icon" data-move="down" type="button" aria-label="Move this rule down">↓</button>
                    <button class="icon" data-remove type="button" aria-label="Remove this rule">✕</button>
                </span>
            </div>
            <div class="rule-grid">
                <label class="field">
                    <span class="field-label">Countries <span class="hint">comma separated, any when empty</span></span>
                    <input class="mono" data-field="countries" placeholder="IN, PK" autocomplete="off" spellcheck="false" value="${escapeHTML(countries.join(", "))}">
                </label>
                <label class="field">
                    <span class="field-label">Regions <span class="hint">comma separated, any when empty</span></span>
                    <input class="mono" data-field="regions" placeholder="MH, KA" autocomplete="off" spellcheck="false" value="${escapeHTML(regions.join(", "))}">
                </label>
                <fieldset class="field">
                    <legend class="field-label">Platforms <span class="hint">any when none ticked</span></legend>
                    <div class="checks">${checkMarkup("platforms", PLATFORMS, platforms)}</div>
                </fieldset>
                <fieldset class="field">
                    <legend class="field-label">Devices <span class="hint">any when none ticked</span></legend>
                    <div class="checks">${checkMarkup("devices", DEVICES, devices)}</div>
                </fieldset>
                <label class="field rule-wide">
                    <span class="field-label">Target URL for this rule</span>
                    <input data-field="url" type="url" inputmode="url" placeholder="https://play.google.com/store/apps/details?id=example" value="${escapeHTML(url)}">
                </label>
            </div>
            <p class="rule-summary" data-summary></p>
        </li>
    `;

    return element(html);
}

function showRuleSummary(row) {
    row.querySelector("[data-summary]").textContent = ruleSummary(readRule(row));
}

function showRuleOrder(name) {
    const { add, count, list } = RULE_LISTS[name];
    const rows = [...el(list).children];

    rows.forEach((row, index) => {
        row.querySelector("[data-index]").textContent = `Rule ${index + 1}`;
        row.querySelector('[data-move="up"]').disabled = index === 0;
        row.querySelector('[data-move="down"]').disabled = index === rows.length - 1;
    });

    el(add).disabled = rows.length >= RULE_LIMIT;
    el(count).textContent = rows.length ? ruleCountMessage(rows.length) : NO_RULES_MSG;
}

function addRule(name, rule = {}) {
    const row = ruleRow(rule);

    el(RULE_LISTS[name].list).append(row);
    showRuleSummary(row);
    showRuleOrder(name);

    return row;
}

export function showRules(name, rules) {
    el(RULE_LISTS[name].list).replaceChildren();

    for (const rule of rules) addRule(name, rule);

    showRuleOrder(name);
}

function moveRow(row, direction) {
    const sibling = direction === "up" ? row.previousElementSibling : row.nextElementSibling;

    if (!sibling) return;

    if (direction === "up") sibling.before(row);
    else sibling.after(row);
}

export function wireRules(name) {
    const list = el(RULE_LISTS[name].list);

    el(RULE_LISTS[name].add).addEventListener("click", () => {
        const row = addRule(name);

        row.querySelector('[data-field="countries"]').focus();
    });

    list.addEventListener("click", (event) => {
        const button = event.target.closest("button");

        if (!button) return;

        const row = button.closest(".rule");

        if (button.hasAttribute("data-remove")) row.remove();
        else moveRow(row, button.dataset.move);

        showRuleOrder(name);
        if (button.isConnected) button.focus();
    });

    list.addEventListener("input", (event) => showRuleSummary(event.target.closest(".rule")));
    list.addEventListener("change", (event) => showRuleSummary(event.target.closest(".rule")));

    showRuleOrder(name);
}
