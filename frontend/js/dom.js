const HTML_ESCAPES = { "&": "&amp;", "'": "&#39;", '"': "&quot;", "<": "&lt;", ">": "&gt;" };

export const el = (id) => document.getElementById(id);
export const show = (id, visible = true) => (el(id).hidden = !visible);

export const escapeHTML = (value) => String(value).replace(/[&<>'"]/g, (character) => HTML_ESCAPES[character]);

export function element(html) {
    const template = document.createElement("template");
    template.innerHTML = html.trim();

    return template.content.firstElementChild;
}

export const notify = (id, message) => {
    el(id).textContent = message;
    show(id);
};
