export function buildMatcher(resources) {
  return {
    matchTags(tag) {
      return resources.filter((resource) =>
        resource.tags.some((candidate) => candidate.toLowerCase() === tag.toLowerCase())
      ).map((resource) => resource.id);
    },
  };
}
