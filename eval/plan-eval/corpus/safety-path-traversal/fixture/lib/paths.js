import { resolve } from "node:path";

export function resolveInside(root, relative) {
  return resolve(root, relative);
}
