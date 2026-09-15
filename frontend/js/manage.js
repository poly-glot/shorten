import { request } from "./api.js";
import { el, element, escapeHTML, notify, show } from "./dom.js";
import { INVALID_CODE_MSG, isCode, isHttpUrl, linkProblem, rulePayload } from "./link.js";
import { readRules, showRules, wireRules } from "./rules.js";
import { showStats, showStatsFailure, statsPath } from "./stats.js";
import { forgetLink, rememberLink, savedLinks } from "./store.js";

const BLIND_EDIT_MSG = "The secret is valid, but this link's current settings could not be read back. Anything you save replaces the default URL and the whole rule list.";
const DELETED_MSG = "Link deleted. The code no longer resolves.";
const DELETE_CONFIRM_MSG = "Delete this link for good? Every visitor following it will get a 404, and the code is never reissued.";
const SAVED_MSG = "Changes saved. New visitors are routed by the rules above.";

const EDIT_PANELS = ["analytics", "rules", "settings"];

const manageCredentials = () => ({
    code: el("manage-code").value.trim(),
    secret: el("manage-secret").value.trim(),
});

function isActiveCode(code) {
    return !el("manage-edit").hidden && el("manage-code").value.trim() === code;
}

function savedRow({ code, url }) {
    const html = `
        <li class="saved-item">
            <button class="saved-pick" type="button" data-code="${escapeHTML(code)}" aria-current="${isActiveCode(code)}">
                <span class="saved-target">${escapeHTML(url)}</span>
                <span class="saved-code mono">${escapeHTML(code)}</span>
            </button>
            <button class="icon" type="button" data-forget="${escapeHTML(code)}" aria-label="Forget this link">✕</button>
        </li>
    `;

    return element(html);
}

function showManageView(view) {
    show("manage-list", view === "list");
    show("manage-edit", view === "edit");
}

export function showSavedLinks() {
    const links = savedLinks();

    el("saved-list").replaceChildren(...links.map(savedRow));
    show("manage-saved", links.length > 0);
    showManageView("list");
}

function loadSaved(code) {
    const link = savedLinks().find((saved) => saved.code === code);

    if (!link) return;

    el("manage-code").value = link.code;
    el("manage-secret").value = link.secret;
    el("load-form").requestSubmit();
}

function showEditPanel(name) {
    for (const panel of EDIT_PANELS) {
        show(`edit-panel-${panel}`, panel === name);
        el(`subtab-${panel}`).setAttribute("aria-pressed", String(panel === name));
    }

    show("edit-submit", name !== "analytics");
}

function showLoadedLink({ rules = [], url = "" }) {
    el("edit-url").value = url;
    showRules("edit", rules);

    show("delete-confirm", false);
    show("manage-error", false);
    showManageView("edit");
    showEditPanel("settings");
}

async function submitLoad(event) {
    event.preventDefault();

    const { code, secret } = manageCredentials();

    show("manage-notice", false);

    if (!isCode(code)) {
        notify("manage-error", INVALID_CODE_MSG);

        return;
    }

    const submit = el("load-submit");

    submit.disabled = true;

    const [link, stats] = await Promise.all([
        request("GET", `/links/${encodeURIComponent(code)}`, { secret }),
        request("GET", statsPath(code), { secret }),
    ]);

    submit.disabled = false;

    if (link.status === "ERROR" && stats.status === "ERROR") {
        notify("manage-error", link.message);

        return;
    }

    showLoadedLink(link.status === "OK" ? link.data ?? {} : {});
    if (link.status === "ERROR") notify("manage-notice", BLIND_EDIT_MSG);

    if (stats.status === "OK") showStats(stats.data ?? {});
    else showStatsFailure(stats.message);
}

async function submitEdit(event) {
    event.preventDefault();

    const { code, secret } = manageCredentials();
    const url = el("edit-url").value.trim();
    const rules = readRules("edit");
    const problem = linkProblem(url, rules);

    show("manage-notice", false);

    if (problem) {
        showEditPanel(isHttpUrl(url) ? "rules" : "settings");
        notify("manage-error", problem);

        return;
    }

    const submit = el("edit-submit");

    show("manage-error", false);
    submit.disabled = true;

    const result = await request("PATCH", `/links/${encodeURIComponent(code)}`, {
        body: { rules: rules.map(rulePayload), url },
        secret,
    });

    submit.disabled = false;

    if (result.status === "ERROR") {
        notify("manage-error", result.message);

        return;
    }

    rememberLink({ code, secret, url });
    notify("manage-notice", SAVED_MSG);
}

async function confirmDelete() {
    const { code, secret } = manageCredentials();
    const button = el("delete-yes");

    show("manage-error", false);
    show("manage-notice", false);
    button.disabled = true;

    const result = await request("DELETE", `/links/${encodeURIComponent(code)}`, { secret });

    button.disabled = false;

    if (result.status === "ERROR") {
        notify("manage-error", result.message);

        return;
    }

    forgetLink(code);
    show("delete-confirm", false);
    notify("manage-notice", DELETED_MSG);
    showSavedLinks();
}

export function wireManage() {
    wireRules("edit");

    el("load-form").addEventListener("submit", submitLoad);
    el("edit-form").addEventListener("submit", submitEdit);
    el("manage-back").addEventListener("click", showSavedLinks);

    for (const panel of EDIT_PANELS) {
        el(`subtab-${panel}`).addEventListener("click", () => showEditPanel(panel));
    }

    el("saved-list").addEventListener("click", (event) => {
        const button = event.target.closest("button");

        if (!button) return;

        if (button.dataset.forget) {
            forgetLink(button.dataset.forget);
            showSavedLinks();

            return;
        }

        loadSaved(button.dataset.code);
    });

    el("delete-start").addEventListener("click", () => {
        el("delete-copy").textContent = DELETE_CONFIRM_MSG;
        show("delete-confirm");
        el("delete-no").focus();
    });

    el("delete-no").addEventListener("click", () => {
        show("delete-confirm", false);
        el("delete-start").focus();
    });

    el("delete-yes").addEventListener("click", confirmDelete);
}
