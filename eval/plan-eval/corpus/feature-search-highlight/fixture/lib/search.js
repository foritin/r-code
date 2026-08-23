export function search(docs, query) {
  const lowered = query.toLowerCase();
  return docs
    .filter((doc) => doc.text.toLowerCase().includes(lowered))
    .map((doc) => ({ id: doc.id }));
}
