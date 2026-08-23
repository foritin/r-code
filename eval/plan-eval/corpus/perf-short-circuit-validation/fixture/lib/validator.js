export function validateAll(records, rules) {
  const errors = [];
  for (const record of records) {
    for (const rule of rules) {
      const error = rule(record);
      if (error) errors.push(error);
    }
  }
  return { errors };
}
