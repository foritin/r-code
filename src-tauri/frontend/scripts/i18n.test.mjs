import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import process from "node:process";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { chromium } from "playwright-core";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const viteBin = path.join(frontendDir, "node_modules", "vite", "bin", "vite.js");
const localeDir = path.join(frontendDir, "src", "i18n", "locales");

function browserExecutable() {
  const localAppData = process.env.LOCALAPPDATA ?? "";
  const playwrightCache = path.join(localAppData, "ms-playwright");
  const cached = fs.existsSync(playwrightCache)
    ? fs.readdirSync(playwrightCache)
      .filter((entry) => /^chromium-\d+$/.test(entry))
      .sort((left, right) => Number(right.split("-")[1]) - Number(left.split("-")[1]))
      .flatMap((entry) => [
        path.join(playwrightCache, entry, "chrome-win64", "chrome.exe"),
        path.join(playwrightCache, entry, "chrome-linux", "chrome"),
        path.join(
          playwrightCache,
          entry,
          "chrome-mac",
          "Chromium.app",
          "Contents",
          "MacOS",
          "Chromium",
        ),
      ])
      .find((candidate) => fs.existsSync(candidate))
    : undefined;

  return [
    cached,
    path.join(process.env.PROGRAMFILES ?? "", "Google", "Chrome", "Application", "chrome.exe"),
    path.join(process.env.PROGRAMFILES ?? "", "Microsoft", "Edge", "Application", "msedge.exe"),
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "/usr/bin/google-chrome",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
  ].find((candidate) => candidate && fs.existsSync(candidate));
}

async function freePort() {
  return new Promise((resolve, reject) => {
    const socket = net.createServer();
    socket.once("error", reject);
    socket.listen(0, "127.0.0.1", () => {
      const address = socket.address();
      const port = typeof address === "object" && address ? address.port : 0;
      socket.close((error) => error ? reject(error) : resolve(port));
    });
  });
}

async function waitForServer(url, processHandle) {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    if (processHandle.exitCode != null) {
      throw new Error(`Vite exited with ${processHandle.exitCode}`);
    }
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch {
      // The test server is still starting.
    }
    await new Promise((resolve) => setTimeout(resolve, 80));
  }
  throw new Error("Timed out waiting for the frontend i18n test server");
}

function flattenCatalog(value, prefix = "", result = new Map()) {
  for (const [key, child] of Object.entries(value)) {
    const fullKey = prefix ? `${prefix}.${key}` : key;
    if (typeof child === "string") result.set(fullKey, child);
    else flattenCatalog(child, fullKey, result);
  }
  return result;
}

function placeholders(message) {
  return [...message.matchAll(/{{\s*([\w.-]+)(?:\s*,[^}]*)?\s*}}/g)]
    .map((match) => match[1])
    .sort();
}

let server;
let browser;
let baseUrl;

test.before(async () => {
  const port = await freePort();
  baseUrl = `http://127.0.0.1:${port}/`;
  server = spawn(
    process.execPath,
    [viteBin, "--host", "127.0.0.1", "--port", String(port), "--strictPort"],
    {
      cwd: frontendDir,
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    },
  );
  await waitForServer(baseUrl, server);
  browser = await chromium.launch({ executablePath: browserExecutable(), headless: true });
});

test.after(async () => {
  await browser?.close();
  server?.kill();
});

async function openIsolatedPage({
  browserLocale = "en-US",
  storage = {},
  failLocaleRead = false,
  failLocaleWrite = false,
} = {}) {
  const context = await browser.newContext({ locale: browserLocale });
  await context.addInitScript(
    ({ entries, readFails, writeFails }) => {
      window.localStorage.clear();
      for (const [key, value] of Object.entries(entries)) {
        window.localStorage.setItem(key, value);
      }

      const originalGetItem = Storage.prototype.getItem;
      const originalSetItem = Storage.prototype.setItem;
      Storage.prototype.getItem = function getItem(key) {
        if (readFails && key === "r-code.locale") throw new Error("forced locale read failure");
        return originalGetItem.call(this, key);
      };
      Storage.prototype.setItem = function setItem(key, value) {
        if (writeFails && key === "r-code.locale") throw new Error("forced locale write failure");
        return originalSetItem.call(this, key, value);
      };
    },
    {
      entries: storage,
      readFails: failLocaleRead,
      writeFails: failLocaleWrite,
    },
  );
  const page = await context.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  return { context, page };
}

async function localeSnapshot(page, includeStored = true) {
  return page.evaluate(async (readStored) => {
    const locale = await import("/src/i18n/index.ts");
    return {
      appLocale: locale.getAppLocale(),
      htmlLang: document.documentElement.lang,
      htmlDir: document.documentElement.dir,
      stored: readStored ? window.localStorage.getItem(locale.LOCALE_STORAGE_KEY) : null,
    };
  }, includeStored);
}

test("a fresh profile defaults to zh-CN regardless of browser language", async () => {
  const english = await openIsolatedPage({ browserLocale: "en-GB" });
  assert.deepEqual(await localeSnapshot(english.page), {
    appLocale: "zh-CN",
    htmlLang: "zh-CN",
    htmlDir: "ltr",
    stored: "zh-CN",
  });
  await english.context.close();

  const chinese = await openIsolatedPage({ browserLocale: "zh-HK" });
  assert.deepEqual(await localeSnapshot(chinese.page), {
    appLocale: "zh-CN",
    htmlLang: "zh-CN",
    htmlDir: "ltr",
    stored: "zh-CN",
  });
  const languageMapping = await chinese.page.evaluate(async () => {
    const { localeFromLanguages } = await import("/src/i18n/index.ts");
    return {
      preferredChinese: localeFromLanguages(["fr-FR", "zh-Hant-TW", "en-US"]),
      unsupportedLanguage: localeFromLanguages(["de-DE"]),
    };
  });
  assert.deepEqual(languageMapping, {
    preferredChinese: "zh-CN",
    unsupportedLanguage: "zh-CN",
  });
  await chinese.context.close();
});

test("an upgraded profile without a locale keeps the historical zh-CN default", async () => {
  const { context, page } = await openIsolatedPage({
    browserLocale: "en-US",
    storage: { "r-code.theme.mode": "dark" },
  });
  assert.deepEqual(await localeSnapshot(page), {
    appLocale: "zh-CN",
    htmlLang: "zh-CN",
    htmlDir: "ltr",
    stored: "zh-CN",
  });
  await context.close();
});

test("a valid saved locale wins over the browser language", async () => {
  const { context, page } = await openIsolatedPage({
    browserLocale: "zh-CN",
    storage: {
      "r-code.locale": "en-US",
      "r-code.locale-source": "explicit",
      "r-code.theme.mode": "system",
    },
  });
  assert.deepEqual(await localeSnapshot(page), {
    appLocale: "en-US",
    htmlLang: "en-US",
    htmlDir: "ltr",
    stored: "en-US",
  });
  await context.close();
});

test("a locale persisted by the old follow-OS policy resets to zh-CN once", async () => {
  const { context, page } = await openIsolatedPage({
    browserLocale: "en-US",
    storage: { "r-code.locale": "en-US" },
  });
  assert.deepEqual(await localeSnapshot(page), {
    appLocale: "zh-CN",
    htmlLang: "zh-CN",
    htmlDir: "ltr",
    stored: "zh-CN",
  });
  const source = await page.evaluate((key) => window.localStorage.getItem(key), "r-code.locale-source");
  assert.equal(source, "auto");
  await context.close();
});

test("locale storage read and write failures preserve an in-memory language", async () => {
  const readFailure = await openIsolatedPage({
    browserLocale: "en-US",
    failLocaleRead: true,
  });
  // 读取失败时无法拿到保存偏好：按默认语言策略落在 zh-CN，且不落盘。
  assert.deepEqual(await localeSnapshot(readFailure.page, false), {
    appLocale: "zh-CN",
    htmlLang: "zh-CN",
    htmlDir: "ltr",
    stored: null,
  });
  await readFailure.context.close();

  const writeFailure = await openIsolatedPage({
    browserLocale: "en-US",
    failLocaleWrite: true,
  });
  assert.deepEqual(await localeSnapshot(writeFailure.page), {
    appLocale: "zh-CN",
    htmlLang: "zh-CN",
    htmlDir: "ltr",
    stored: null,
  });
  const switched = await writeFailure.page.evaluate(async () => {
    const locale = await import("/src/i18n/index.ts");
    await locale.setAppLocale("en-US");
    return {
      appLocale: locale.getAppLocale(),
      htmlLang: document.documentElement.lang,
      stored: window.localStorage.getItem(locale.LOCALE_STORAGE_KEY),
    };
  });
  assert.deepEqual(switched, {
    appLocale: "en-US",
    htmlLang: "en-US",
    stored: null,
  });
  await writeFailure.context.close();
});

test("the Settings language control switches React text, html lang, and persistence immediately", async () => {
  const { context, page } = await openIsolatedPage({
    browserLocale: "en-US",
    storage: { "r-code.locale": "en-US", "r-code.locale-source": "explicit" },
  });

  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().setSettingsPane("preferences");
  });
  const englishSelect = page.getByRole("combobox", { name: "Interface language" });
  await englishSelect.waitFor({ state: "visible" });
  assert.equal(await englishSelect.inputValue(), "en-US");

  await englishSelect.selectOption("zh-CN");
  await page.getByRole("heading", { name: "设置", exact: true }).waitFor({ state: "visible" });
  await page.getByRole("button", { name: "对话", exact: true }).waitFor({ state: "visible" });
  assert.deepEqual(await localeSnapshot(page), {
    appLocale: "zh-CN",
    htmlLang: "zh-CN",
    htmlDir: "ltr",
    stored: "zh-CN",
  });

  const chineseSelect = page.getByRole("combobox", { name: "界面语言" });
  assert.equal(await chineseSelect.inputValue(), "zh-CN");
  await chineseSelect.selectOption("en-US");
  await page.getByRole("heading", { name: "Settings", exact: true }).waitFor({ state: "visible" });
  assert.deepEqual(await localeSnapshot(page), {
    appLocale: "en-US",
    htmlLang: "en-US",
    htmlDir: "ltr",
    stored: "en-US",
  });
  await context.close();
});

test("zh-CN and en-US catalogs have identical leaf keys and interpolation placeholders", () => {
  const zhCN = flattenCatalog(JSON.parse(fs.readFileSync(path.join(localeDir, "zh-CN.json"), "utf8")));
  const enUS = flattenCatalog(JSON.parse(fs.readFileSync(path.join(localeDir, "en-US.json"), "utf8")));
  const zhKeys = [...zhCN.keys()].sort();
  const enKeys = [...enUS.keys()].sort();

  assert.deepEqual(enKeys, zhKeys, "locale catalogs must expose exactly the same leaf keys");
  assert.ok(zhKeys.length > 0, "locale catalogs must not be empty");
  for (const key of zhKeys) {
    assert.notEqual(zhCN.get(key).trim(), "", `${key} must have a zh-CN translation`);
    assert.notEqual(enUS.get(key).trim(), "", `${key} must have an en-US translation`);
    assert.deepEqual(
      placeholders(enUS.get(key)),
      placeholders(zhCN.get(key)),
      `${key} must use the same interpolation placeholders in both catalogs`,
    );
  }

  const platformNeutralNotificationKeys = [
    "settings.notifications.status.denied.description",
    "errors.notifications.permission_request_failed",
  ];
  for (const key of platformNeutralNotificationKeys) {
    assert.match(zhCN.get(key), /操作系统的通知设置/, `${key} must cover every desktop OS in zh-CN`);
    assert.match(
      enUS.get(key),
      /operating system's notification settings/,
      `${key} must cover every desktop OS in en-US`,
    );
    assert.doesNotMatch(zhCN.get(key), /Windows|macOS|Linux/i);
    assert.doesNotMatch(enUS.get(key), /Windows|macOS|Linux/i);
  }
});

test("Intl helpers use the active locale and handle invalid dates", async () => {
  const { context, page } = await openIsolatedPage({ browserLocale: "en-US" });
  const formatted = await page.evaluate(async () => {
    const locale = await import("/src/i18n/index.ts");
    const dateOptions = {
      timeZone: "UTC",
      year: "numeric",
      month: "long",
      day: "numeric",
    };
    const numberOptions = { minimumFractionDigits: 2, maximumFractionDigits: 2 };
    const relativeOptions = { numeric: "auto" };
    const timestamp = Date.UTC(2026, 7, 26, 12, 30, 0);

    await locale.setAppLocale("en-US");
    const english = {
      date: locale.formatDateTime(timestamp, dateOptions),
      number: locale.formatNumber(1234567.89, numberOptions),
      relative: locale.formatRelativeTime(-1, "day", relativeOptions),
    };
    const expectedEnglish = {
      date: new Intl.DateTimeFormat("en-US", dateOptions).format(timestamp),
      number: new Intl.NumberFormat("en-US", numberOptions).format(1234567.89),
      relative: new Intl.RelativeTimeFormat("en-US", relativeOptions).format(-1, "day"),
    };

    await locale.setAppLocale("zh-CN");
    const chinese = {
      date: locale.formatDateTime(timestamp, dateOptions),
      number: locale.formatNumber(1234567.89, numberOptions),
      relative: locale.formatRelativeTime(-1, "day", relativeOptions),
    };
    const expectedChinese = {
      date: new Intl.DateTimeFormat("zh-CN", dateOptions).format(timestamp),
      number: new Intl.NumberFormat("zh-CN", numberOptions).format(1234567.89),
      relative: new Intl.RelativeTimeFormat("zh-CN", relativeOptions).format(-1, "day"),
    };

    return {
      english,
      expectedEnglish,
      chinese,
      expectedChinese,
      invalidDate: locale.formatDateTime("not-a-date", dateOptions),
    };
  });

  assert.deepEqual(formatted.english, formatted.expectedEnglish);
  assert.deepEqual(formatted.chinese, formatted.expectedChinese);
  assert.equal(formatted.invalidDate, "—");
  assert.notEqual(formatted.english.date, formatted.chinese.date);
  assert.notEqual(formatted.english.relative, formatted.chinese.relative);
  await context.close();
});

test("the IPC error adapter localizes structured errors without exposing technical detail", async () => {
  const { context, page } = await openIsolatedPage({ browserLocale: "en-US" });
  const result = await page.evaluate(async () => {
    let nextError;
    window.__TAURI_INTERNALS__ = {
      invoke: async () => {
        throw nextError;
      },
    };
    const ipc = await import("/src/lib/ipc.ts?i18n-error-adapter-test");
    const locale = await import("/src/i18n/index.ts");

    const capture = async (payload) => {
      nextError = payload;
      try {
        await ipc.ping();
        throw new Error("IPC test command unexpectedly succeeded");
      } catch (error) {
        return {
          constructorName: error.constructor.name,
          name: error.name,
          message: error.message,
          code: error.code,
          args: error.args ?? null,
          limit: error.limit ?? null,
          debugDetail: error.debugDetail ?? null,
          copied: typeof error.copyTechnicalDetail === "function"
            ? error.copyTechnicalDetail()
            : null,
          hasCopyMethod: typeof error.copyTechnicalDetail === "function",
        };
      }
    };

    await locale.setAppLocale("en-US");
    const structuredEnglish = await capture({
      code: "browser.origin_permission_required",
      args: { origin: "https://example.com" },
      debug_detail: "authorization=secret-token\ninternal stack",
    });
    const unknown = await capture({
      code: "future.unmapped_error",
      args: { ignored: "value" },
      debug_detail: "unknown technical detail",
    });
    const legacy = await capture({
      code: "LEGACY_LIMIT",
      message: "Legacy host message",
      limit: 5,
      debug_detail: "legacy debug must be ignored",
    });

    await locale.setAppLocale("zh-CN");
    const structuredChinese = await capture({
      code: "automation.capability_denied",
      args: { capability: "shell" },
    });
    delete window.__TAURI_INTERNALS__;
    return { structuredEnglish, structuredChinese, unknown, legacy };
  });

  assert.deepEqual(result.structuredEnglish, {
    constructorName: "UserFacingIpcError",
    name: "UserFacingIpcError",
    message: "Permission is required to browse https://example.com.",
    code: "browser.origin_permission_required",
    args: { origin: "https://example.com" },
    limit: null,
    debugDetail: "authorization=secret-token\ninternal stack",
    copied: "authorization=secret-token\ninternal stack",
    hasCopyMethod: true,
  });
  assert.ok(!result.structuredEnglish.message.includes("secret-token"));
  assert.deepEqual(result.structuredChinese, {
    constructorName: "UserFacingIpcError",
    name: "UserFacingIpcError",
    message: "此自动化无权使用 shell。",
    code: "automation.capability_denied",
    args: { capability: "shell" },
    limit: null,
    debugDetail: null,
    copied: null,
    hasCopyMethod: true,
  });
  assert.deepEqual(result.unknown, {
    constructorName: "UserFacingIpcError",
    name: "UserFacingIpcError",
    message: "R-Code couldn't complete that action. Try again or copy the technical details for support.",
    code: "future.unmapped_error",
    args: { ignored: "value" },
    limit: null,
    debugDetail: "unknown technical detail",
    copied: "unknown technical detail",
    hasCopyMethod: true,
  });
  assert.deepEqual(result.legacy, {
    constructorName: "IpcCommandError",
    name: "IpcCommandError",
    message: "Legacy host message",
    code: "LEGACY_LIMIT",
    args: null,
    limit: 5,
    debugDetail: null,
    copied: null,
    hasCopyMethod: false,
  });
  await context.close();
});
