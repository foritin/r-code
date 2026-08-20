export function buildIndex(posts) {
  const index = new Map();
  for (const post of posts) index.set(post.slug, post.id);
  return index;
}
