import { buildMatcher } from "./matcher.js";

export function buildFeed(resources, requested) {
  const matcher = buildMatcher(resources);
  return requested.flatMap((tag) => matcher.matchTags(tag));
}
