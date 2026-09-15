export const codeList = (value) => value.toUpperCase().split(",").map((code) => code.trim()).filter(Boolean);
export const isoDay = (date) => date.toISOString().slice(0, 10);
