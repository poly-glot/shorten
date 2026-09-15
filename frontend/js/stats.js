import { el, element, escapeHTML, notify, show } from "./dom.js";
import { isoDay } from "./format.js";

const DAY_MS = 86400000;
const OTHER = "other";
const SEGMENT_PARTS = 4;
const STATS_WINDOW_DAYS = 90;
const TOP_SEGMENT_LIMIT = 25;
const UNKNOWN = "XX";

const EMPTY_DAYS_MSG = "No clicks recorded yet. Clicks are rolled up once a night, so today's traffic appears tomorrow.";

export const splitSegment = (segment) => {
    const parts = segment.split("|");
    const [country, region, platform, device] = parts.length === SEGMENT_PARTS ? parts : [UNKNOWN, UNKNOWN, OTHER, OTHER];

    return { country, device, platform, region };
};

export function topSegments(days, limit) {
    const totals = new Map();

    for (const day of days) {
        for (const [segment, clicks] of Object.entries(day.seg ?? {})) {
            totals.set(segment, (totals.get(segment) ?? 0) + Number(clicks));
        }
    }

    return [...totals.entries()]
        .map(([segment, clicks]) => ({ clicks, ...splitSegment(segment) }))
        .sort((first, second) => second.clicks - first.clicks)
        .slice(0, limit);
}

export const byDateDescending = (days) => [...days].sort((first, second) => second.date.localeCompare(first.date));

export function statsPath(code) {
    const today = new Date();
    const start = new Date(today.getTime() - (STATS_WINDOW_DAYS - 1) * DAY_MS);

    const range = `from=${isoDay(start)}&to=${isoDay(today)}`;

    return `/links/${encodeURIComponent(code)}/stats?${range}`;
}

function dayRow({ clicks, date }) {
    return element(`<tr><td class="mono">${escapeHTML(date)}</td><td>${Number(clicks)}</td></tr>`);
}

function segmentRow({ clicks, country, device, platform, region }) {
    const cells = [country, region, platform, device].map((value) => `<td class="mono">${escapeHTML(value)}</td>`).join("");

    return element(`<tr>${cells}<td>${clicks}</td></tr>`);
}

function emptyRow(columns, message) {
    return element(`<tr><td colspan="${columns}">${escapeHTML(message)}</td></tr>`);
}

export function showStats({ days = [], total_90d = 0 }) {
    const ordered = byDateDescending(days);
    const segments = topSegments(days, TOP_SEGMENT_LIMIT);

    el("stats-total").textContent = `${total_90d} clicks in the last ${STATS_WINDOW_DAYS} days.`;
    el("stats-days").replaceChildren(...(ordered.length ? ordered.map(dayRow) : [emptyRow(2, EMPTY_DAYS_MSG)]));
    el("stats-segments").replaceChildren(...(segments.length ? segments.map(segmentRow) : [emptyRow(5, EMPTY_DAYS_MSG)]));

    show("stats-error", false);
}

export function showStatsFailure(message) {
    el("stats-total").textContent = "";
    el("stats-days").replaceChildren();
    el("stats-segments").replaceChildren();
    notify("stats-error", message);
}
