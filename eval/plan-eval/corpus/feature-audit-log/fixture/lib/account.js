export function makeAccount(balance) {
  return { balance };
}

export function transfer(from, to, amount) {
  if (from.balance < amount) throw new Error("insufficient");
  from.balance -= amount;
  to.balance += amount;
  return true;
}
