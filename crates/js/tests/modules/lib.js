import { greeting } from "./dep.js";
export function greet(name) { return `${greeting} ${name}`; }
export const meta = import.meta.url.endsWith("/lib.js");
