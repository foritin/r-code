import { setQuota } from "./quota.js";

export function registerTenant(tenant, quota) {
  return { ...tenant, limits: setQuota(tenant.limits ?? { quota: 0 }, quota) };
}
