import i18n, { type TFunction } from "i18next";
import { initReactI18next } from "react-i18next";
import enUS from "./locales/en-US.json";
import zhCN from "./locales/zh-CN.json";

export const APP_LOCALES = ["zh-CN", "en-US"] as const;
export type AppLocale = (typeof APP_LOCALES)[number];

export const LOCALE_STORAGE_KEY = "r-code.locale";
const R_CODE_STORAGE_PREFIX = "r-code.";

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

function browserLanguages(): readonly string[] {
  if (typeof navigator === "undefined") return [];
  return navigator.languages?.length ? navigator.languages : [navigator.language];
}

export function localeFromLanguages(languages: readonly string[]): AppLocale {
  return languages.some((language) => language.toLowerCase().startsWith("zh"))
    ? "zh-CN"
    : "en-US";
}

function hasExistingRCodePreferences(storage: LocaleStorage): boolean {
  for (let index = 0; index < storage.length; index += 1) {
    const key = storage.key(index);
    if (key?.startsWith(R_CODE_STORAGE_PREFIX) && key !== LOCALE_STORAGE_KEY) return true;
  }
  return false;
}

/**
 * Upgrades keep the historical Chinese default. A genuinely empty R-Code
 * profile follows the OS language, then persists that decision immediately.
 */
export function resolveInitialLocale(
  storage: LocaleStorage | null = browserStorage(),
  languages: readonly string[] = browserLanguages(),
): AppLocale {
  if (!storage) return localeFromLanguages(languages);

  let locale: AppLocale;
  try {
    const saved = storage.getItem(LOCALE_STORAGE_KEY);
    if (isAppLocale(saved)) return saved;

    locale = saved !== null || hasExistingRCodePreferences(storage)
      ? "zh-CN"
      : localeFromLanguages(languages);
  } catch {
    return localeFromLanguages(languages);
  }

  try {
    storage.setItem(LOCALE_STORAGE_KEY, locale);
  } catch {
    // The selected locale still applies for this session when persistence fails.
  }
  return locale;
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
