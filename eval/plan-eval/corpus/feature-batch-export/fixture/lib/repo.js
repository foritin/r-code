export function makeRepo() {
  const items = [];
  return {
    add(item) { items.push(item); return items.length; },
    all() { return [...items]; },
  };
}
