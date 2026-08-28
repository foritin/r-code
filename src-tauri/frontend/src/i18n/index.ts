import i18n, { type TFunction } from "i18next";
import { initReactI18next } from "react-i18next";
import enUS from "./locales/en-US.json";
import zhCN from "./locales/zh-CN.json";

export const APP_LOCALES = ["zh-CN", "en-US"] as const;
export type AppLocale = (typeof APP_LOCALES)[number];

export const LOCALE_STORAGE_KEY = "r-code.locale";
export const LOCALE_SOURCE_KEY = "r-code.locale-source";

interface LocaleStorage {
  readonly length: number;
  getItem(key: string): string | null;
  key(index: number): string | null;
  setItem(key: string, value: string): void;
}

function isAppLocale(value: string | null | undefined): value is AppLocale {
  return value === "zh-CN" || value === "en-US";
}

function browserStorage(): LocaleStorage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

/**
 * 2026-08-28 产品决定：默认语言始终为中文（zh-CN）。
 * 系统/浏览器语言不再决定初始语言；外观页显式切换并保存是唯一的换语入口。
 */
export function localeFromLanguages(_languages: readonly string[]): AppLocale {
  return "zh-CN";
}

/**
 * 初始语言解析：
 * - 显式保存过（`r-code.locale-source` = "explicit"）→ 跟随用户选择；
 * - 旧版"跟随系统"策略自动持久化的 locale（无 source 标记）→ 一次性重置为 zh-CN，
 *   否则 2026-08-28 的默认中文策略对存量档案永远不生效（旧策略首启即落盘）；
 * - 无存档 → zh-CN。除显式切换外，结果立即持久化。
 */
export function resolveInitialLocale(
  storage: LocaleStorage | null = browserStorage(),
): AppLocale {
  if (!storage) return "zh-CN";

  let saved: string | null = null;
  let source: string | null = null;
  try {
    saved = storage.getItem(LOCALE_STORAGE_KEY);
    source = storage.getItem(LOCALE_SOURCE_KEY);
  } catch {
    return "zh-CN";
  }
  if (isAppLocale(saved) && source === "explicit") return saved;

  try {
    storage.setItem(LOCALE_STORAGE_KEY, "zh-CN");
    storage.setItem(LOCALE_SOURCE_KEY, "auto");
  } catch {
    // The selected locale still applies for this session when persistence fails.
  }
  return "zh-CN";
}

function normalizeLocale(language: string | null | undefined): AppLocale {
  return language?.toLowerCase().startsWith("en") ? "en-US" : "zh-CN";
}

function applyDocumentLocale(locale: AppLocale): void {
  if (typeof document === "undefined") return;
  document.documentElement.lang = locale;
  document.documentElement.dir = "ltr";
}

const initialLocale = resolveInitialLocale();

void i18n.use(initReactI18next).init({
  resources: {
    "zh-CN": { translation: zhCN },
    "en-US": { translation: enUS },
  },
  lng: initialLocale,
  fallbackLng: "zh-CN",
  supportedLngs: APP_LOCALES,
  load: "currentOnly",
  initAsync: false,
  interpolation: { escapeValue: false },
  react: { useSuspense: false },
});

applyDocumentLocale(initialLocale);
i18n.on("languageChanged", (language) => applyDocumentLocale(normalizeLocale(language)));

export function getAppLocale(): AppLocale {
  return normalizeLocale(i18n.resolvedLanguage ?? i18n.language);
}

export async function setAppLocale(locale: AppLocale): Promise<void> {
  const storage = browserStorage();
  try {
    storage?.setItem(LOCALE_STORAGE_KEY, locale);
    // 显式切换打上标记，避免被「旧策略残留重置」逻辑误重置。
    storage?.setItem(LOCALE_SOURCE_KEY, "explicit");
  } catch {
    // Restricted WebViews still keep the selected language for this session.
  }
  await i18n.changeLanguage(locale);
}

/** Non-React translation entry for stores, IPC adapters, and native bridges. */
export const t: TFunction = i18n.t.bind(i18n);

export function formatDateTime(
  value: Date | number | string,
  options?: Intl.DateTimeFormatOptions,
): string {
  const date = value instanceof Date ? value : new Date(value);
  if (Number.isNaN(date.getTime())) return "—";
  return new Intl.DateTimeFormat(getAppLocale(), options).format(date);
}

export function formatNumber(value: number, options?: Intl.NumberFormatOptions): string {
  return new Intl.NumberFormat(getAppLocale(), options).format(value);
}

export function formatRelativeTime(
  value: number,
  unit: Intl.RelativeTimeFormatUnit,
  options?: Intl.RelativeTimeFormatOptions,
): string {
  return new Intl.RelativeTimeFormat(getAppLocale(), options).format(value, unit);
}

export default i18n;
