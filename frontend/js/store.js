const STORAGE_KEY = "shorten.links";

export function savedLinks() {
    try {
        const parsed = JSON.parse(localStorage.getItem(STORAGE_KEY));

        return Array.isArray(parsed) ? parsed : [];
    } catch {
        return [];
    }
}

function writeSaved(links) {
    try {
        localStorage.setItem(STORAGE_KEY, JSON.stringify(links));
    } catch {
        return;
    }
}

export function rememberLink({ code, secret, url }) {
    const links = savedLinks().filter((link) => link.code !== code);

    links.unshift({ code, secret, url });
    writeSaved(links);
}

export function forgetLink(code) {
    writeSaved(savedLinks().filter((link) => link.code !== code));
}
