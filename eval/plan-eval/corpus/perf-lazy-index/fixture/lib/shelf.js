import { makeCatalog } from "./catalog.js";

export function buildShelf(collections) {
  return collections.map((items) => makeCatalog(items));
}
