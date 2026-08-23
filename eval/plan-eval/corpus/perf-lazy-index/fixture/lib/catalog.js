export function makeCatalog(items) {
  const byTag = new Map();
  for (const item of items) {
    for (const tag of item.tags) {
      const bucket = byTag.get(tag) ?? [];
      bucket.push(item.id);
      byTag.set(tag, bucket);
    }
  }
  return {
    findByTag(tag) { return byTag.get(tag) ?? []; },
    indexBuiltAt: Date.now(),
  };
}
