export function createLedger() {
  const balance = { amount: 100 };
  return {
    charge(amount) {
      balance.amount -= amount;
      return { charged: amount, remaining: balance.amount };
    },
    balance() { return balance.amount; },
  };
}
