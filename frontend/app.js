import { wireCreate } from "./js/create.js";
import { el, show } from "./js/dom.js";
import { showSavedLinks, wireManage } from "./js/manage.js";

function showPanel(name) {
    const creating = name === "create";

    show("create", creating);
    show("manage", !creating);
    el("tab-create").setAttribute("aria-pressed", String(creating));
    el("tab-manage").setAttribute("aria-pressed", String(!creating));

    if (!creating) showSavedLinks();
}

function boot() {
    el("tab-create").addEventListener("click", () => showPanel("create"));
    el("tab-manage").addEventListener("click", () => showPanel("manage"));

    wireCreate();
    wireManage();
}

boot();
