const BASE = "/api";
const NETWORK_MSG = "The API did not answer. Check your connection and try again.";
const UNEXPECTED_MSG = "Something went wrong. Try again in a moment.";

async function readJSON(response) {
    try {
        return await response.json();
    } catch {
        return null;
    }
}

export async function request(method, path, { body, secret } = {}) {
    const headers = {};

    if (body) headers["Content-Type"] = "application/json";
    if (secret) headers["X-Manage-Secret"] = secret;

    try {
        const response = await fetch(BASE + path, { body: body ? JSON.stringify(body) : undefined, headers, method });
        const data = await readJSON(response);

        if (response.ok) return { data, status: "OK" };

        return { message: data?.error?.message ?? UNEXPECTED_MSG, status: "ERROR" };
    } catch {
        return { message: NETWORK_MSG, status: "ERROR" };
    }
}
