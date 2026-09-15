import { request } from "./api.js";
import { el, notify, show } from "./dom.js";
import { linkProblem, rulePayload } from "./link.js";
import { readRules, showRules, wireRules } from "./rules.js";
import { rememberLink } from "./store.js";

const COPIED_MSG = "Copied to the clipboard.";
const COPY_FAILED_MSG = "Could not reach the clipboard. Select the text and copy it by hand.";
const SECRET_WARNING_MSG = "Shown once — save it now. We store only a hash of this secret, so it cannot be shown again or recovered.";

function showCreated({ manage_secret, short_url }) {
    el("create-url").value = "";
    showRules("create", []);

    el("secret-warning").textContent = SECRET_WARNING_MSG;
    el("result-url").value = short_url;
    el("result-secret").value = manage_secret;
    el("copy-note").textContent = "";

    show("create-error", false);
    show("create-result");
    el("create-result").scrollIntoView({ block: "nearest" });
}

async function copyInto(sourceId) {
    const { value } = el(sourceId);

    try {
        await navigator.clipboard.writeText(value);

        return COPIED_MSG;
    } catch {
        return COPY_FAILED_MSG;
    }
}

function wireCopy(buttonId, sourceId) {
    el(buttonId).addEventListener("click", async () => {
        el("copy-note").textContent = await copyInto(sourceId);
    });
}

async function submitCreate(event) {
    event.preventDefault();

    const url = el("create-url").value.trim();
    const rules = readRules("create");
    const problem = linkProblem(url, rules);

    if (problem) {
        show("create-result", false);
        notify("create-error", problem);

        return;
    }

    const submit = el("create-submit");

    show("create-error", false);
    submit.disabled = true;

    const result = await request("POST", "/links", { body: { rules: rules.map(rulePayload), url } });

    submit.disabled = false;

    if (result.status === "ERROR") {
        show("create-result", false);
        notify("create-error", result.message);

        return;
    }

    rememberLink({ code: result.data.code, secret: result.data.manage_secret, url });
    showCreated(result.data);
}

export function wireCreate() {
    wireRules("create");
    wireCopy("copy-url", "result-url");
    wireCopy("copy-secret", "result-secret");

    el("create-form").addEventListener("submit", submitCreate);
}
