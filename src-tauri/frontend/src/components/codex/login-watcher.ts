export const CODEX_LOGIN_POLL_INTERVAL_MS = 2_000;
export const CODEX_LOGIN_WAIT_TIMEOUT_MS = 3 * 60_000;
export const CODEX_LOGIN_WAIT_MINUTES = CODEX_LOGIN_WAIT_TIMEOUT_MS / 60_000;

/**
 * 返回下一次登录状态探测前的等待时间；`null` 表示本轮有限等待已经结束。
 *
 * 临近截止时间时缩短最后一次等待，避免固定 interval 越过超时边界。调用方应在
 * 上一次探测结束后再调用它，从而不会堆叠并发的 `codex login status` 进程。
 */
export function nextCodexLoginPollDelay(
  startedAtMs: number,
  nowMs = Date.now(),
): number | null {
  const elapsed = Math.max(0, nowMs - startedAtMs);
  const remaining = CODEX_LOGIN_WAIT_TIMEOUT_MS - elapsed;
  if (remaining <= 0) return null;
  return Math.min(CODEX_LOGIN_POLL_INTERVAL_MS, remaining);
}
