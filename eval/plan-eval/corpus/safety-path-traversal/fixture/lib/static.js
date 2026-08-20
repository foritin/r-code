import { readFileSync } from "node:fs";
import { resolveInside } from "./paths.js";

export function readAsset(root, relative) {
  const path = resolveInside(root, relative);
  return path ? readFileSync(path, "utf8") : null;
}
