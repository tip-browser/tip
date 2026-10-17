import { aName } from "./cycle-b.js";
export function b() { return "b:" + aName(); }
export const name = "A";
