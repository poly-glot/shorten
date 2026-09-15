const CODE_PATTERN = /^[A-Za-z0-9_-]{10}$/;
const LIST_LIMIT = 30;
export const RULE_LIMIT = 20;

export const INVALID_CODE_MSG = "A code is exactly 10 characters from A-Z, a-z, 0-9, hyphen and underscore.";
const INVALID_URL_MSG = "The default target must be an http:// or https:// URL.";
const LIST_LIMIT_MSG = `takes at most ${LIST_LIMIT} values.`;
const RULE_LIMIT_MSG = `A link takes at most ${RULE_LIMIT} rules.`;

export const isCode = (value) => CODE_PATTERN.test(value);

export function isHttpUrl(value) {
    try {
        const { protocol } = new URL(value);

        return protocol === "http:" || protocol === "https:";
    } catch {
        return false;
    }
}

export function rulePayload({ countries, devices, platforms, regions, url }) {
    const payload = { url };

    if (countries.length) payload.countries = countries;
    if (devices.length) payload.devices = devices;
    if (platforms.length) payload.platforms = platforms;
    if (regions.length) payload.regions = regions;

    return payload;
}

function ruleProblem({ countries, regions, url }, position) {
    if (!isHttpUrl(url)) return `Rule ${position} needs an http:// or https:// target URL.`;
    if (countries.length > LIST_LIMIT) return `Rule ${position}: countries ${LIST_LIMIT_MSG}`;
    if (regions.length > LIST_LIMIT) return `Rule ${position}: regions ${LIST_LIMIT_MSG}`;

    return "";
}

export function linkProblem(url, rules) {
    if (!isHttpUrl(url)) return INVALID_URL_MSG;
    if (rules.length > RULE_LIMIT) return RULE_LIMIT_MSG;

    for (const [index, rule] of rules.entries()) {
        const problem = ruleProblem(rule, index + 1);

        if (problem) return problem;
    }

    return "";
}
