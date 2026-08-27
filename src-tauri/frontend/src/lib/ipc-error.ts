import { t } from "../i18n";

export interface UserFacingErrorPayload {
  code: string;
  args?: Record<string, unknown>;
  debug_detail?: string;
}

export class UserFacingIpcError extends Error {
  readonly code: string;
  readonly args: Record<string, unknown>;
  readonly debugDetail?: string;

  constructor(payload: UserFacingErrorPayload) {
    const args = payload.args ?? {};
    const translated = t(`errors.${payload.code}`, {
      ...args,
      defaultValue: t("errors.unknown"),
    });
    super(String(translated));
    this.name = "UserFacingIpcError";
    this.code = payload.code;
    this.args = args;
    this.debugDetail = payload.debug_detail;
  }

  copyTechnicalDetail(): string | null {
    return this.debugDetail ?? null;
  }
}

function userFacingErrorPayload(cause: unknown): UserFacingErrorPayload | null {
  let candidate: Record<string, unknown>;
  if (typeof cause === "string" || cause instanceof Error) {
    const serialized = typeof cause === "string" ? cause : cause.message;
    try {
      const decoded: unknown = JSON.parse(serialized);
      if (typeof decoded !== "object" || decoded == null || Array.isArray(decoded)) return null;
      candidate = decoded as Record<string, unknown>;
    } catch {
      return null;
    }
  } else if (typeof cause === "object" && cause != null && !Array.isArray(cause)) {
    candidate = cause as Record<string, unknown>;
  } else {
    return null;
  }

  if (typeof candidate.code !== "string" || typeof candidate.message === "string") return null;
  const args = candidate.args;
  if (args != null && (typeof args !== "object" || Array.isArray(args))) return null;
  return {
    code: candidate.code,
    ...(args ? { args: args as Record<string, unknown> } : {}),
    ...(typeof candidate.debug_detail === "string"
      ? { debug_detail: candidate.debug_detail }
      : {}),
  };
}

export function toUserFacingIpcError(cause: unknown): UserFacingIpcError | null {
  const payload = userFacingErrorPayload(cause);
  return payload ? new UserFacingIpcError(payload) : null;
}
