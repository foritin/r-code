import { validateAll } from "./validator.js";

export function runBatch(records, rules, options = {}) {
  return validateAll(records, rules);
}
