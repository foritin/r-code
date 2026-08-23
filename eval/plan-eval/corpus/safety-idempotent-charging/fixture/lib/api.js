import { createLedger } from "./ledger.js";

export function createApi() {
  const ledger = createLedger();
  return {
    handleOrder(order) {
      return ledger.charge(order.amount);
    },
  };
}
