import { retry } from "./retry.js";

export async function uploadAll(items, send) {
  for (const item of items) {
    await retry(() => send(item));
  }
  return { uploaded: items.length };
}
