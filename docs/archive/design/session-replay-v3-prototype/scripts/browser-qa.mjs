import { chromium } from "playwright";

const baseURL = process.env.REPLAY_URL ?? "http://127.0.0.1:4173";
const browser = await chromium.launch({ headless: true });
const consoleErrors = [];

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

async function settle(page) {
  await page.waitForLoadState("networkidle");
  await page.evaluate(() => document.fonts?.ready);
}

try {
  const desktop = await browser.newPage({ viewport: { width: 1672, height: 941 }, deviceScaleFactor: 1 });
  desktop.on("console", (message) => {
    if (message.type() === "error") consoleErrors.push(message.text());
  });
  desktop.on("pageerror", (error) => consoleErrors.push(error.message));

  await desktop.goto(baseURL);
  await settle(desktop);
  assert(await desktop.getByText("先看结果").isVisible(), "Recap depth navigation is not visible");
  assert(await desktop.getByText("重要变化").isVisible(), "Result-first summary is missing");
  assert(await desktop.getByLabel("Evidence for selected moment").isVisible(), "Evidence drawer is missing");
  await desktop.screenshot({ path: "qa/preview-recap.png" });

  await desktop.getByRole("button", { name: /播放 \d+ 秒回顾/ }).click();
  await desktop.waitForTimeout(750);
  assert(await desktop.getByRole("button", { name: "暂停" }).isVisible(), "Playback did not start");
  await desktop.getByRole("button", { name: "暂停" }).click();

  await desktop.getByRole("button", { name: /探索/ }).click();
  assert(await desktop.getByPlaceholder("搜索事件、应用或证据").isVisible(), "Explore search is missing");
  await desktop.getByRole("button", { name: "哪些尚未验证？" }).click();
  assert((await desktop.locator(".event-list > button").count()) > 0, "Unverified filter returned no events");
  await desktop.waitForTimeout(180);
  await desktop.screenshot({ path: "qa/preview-explore.png" });

  await desktop.getByRole("button", { name: "核验 查看证据" }).click();
  assert(await desktop.getByText("Replay evidence receipt").isVisible(), "Verification receipt is missing");
  await desktop.getByPlaceholder("为什么发生这一步？").last().fill("这项主张由什么支持？");
  await desktop.getByRole("button", { name: "Ask replay" }).last().click();
  assert(await desktop.getByText("基于记录的回答").last().isVisible(), "Ask Replay did not return an evidence-bounded answer");
  await desktop.waitForTimeout(180);
  await desktop.screenshot({ path: "qa/preview-verify.png" });

  await desktop.locator(".session-title").click();
  await desktop.getByRole("button", { name: /采购审批/ }).click();
  assert(await desktop.getByText(/采购申请/).first().isVisible(), "Scenario switch did not update content");
  await desktop.getByRole("button", { name: "真实时间" }).click();
  assert(await desktop.getByRole("button", { name: "真实时间" }).getAttribute("class") === "active", "Real-time projection did not activate");

  const tablet = await browser.newPage({ viewport: { width: 1024, height: 768 }, deviceScaleFactor: 1 });
  tablet.on("console", (message) => {
    if (message.type() === "error") consoleErrors.push(message.text());
  });
  await tablet.goto(baseURL);
  await settle(tablet);
  assert(await tablet.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth), "Tablet viewport has horizontal overflow");
  await tablet.screenshot({ path: "qa/preview-tablet.png" });

  const mobile = await browser.newPage({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 1 });
  mobile.on("console", (message) => {
    if (message.type() === "error") consoleErrors.push(message.text());
  });
  await mobile.goto(baseURL);
  await settle(mobile);
  assert(await mobile.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth), "Mobile viewport has horizontal overflow");
  assert(await mobile.getByRole("button", { name: /播放 \d+ 秒回顾/ }).isVisible(), "Mobile primary playback action is not visible");
  await mobile.screenshot({ path: "qa/preview-mobile.png" });
  await mobile.locator(".depth-nav button").nth(2).click();
  assert(await mobile.getByText("Replay evidence receipt").isVisible(), "Mobile verification receipt is not reachable");
  assert(await mobile.getByPlaceholder("为什么发生这一步？").last().isVisible(), "Mobile evidence question input is not reachable");

  const comparison = await browser.newPage({ viewport: { width: 1720, height: 1000 }, deviceScaleFactor: 1 });
  await comparison.goto(new URL("../qa/comparison.html", import.meta.url).href);
  await comparison.waitForFunction(() => Array.from(document.images).every((image) => image.complete && image.naturalWidth > 0));
  await comparison.screenshot({ path: "qa/preview-comparison.png", fullPage: true });

  assert(consoleErrors.length === 0, `Console errors: ${consoleErrors.join(" | ")}`);
  process.stdout.write(JSON.stringify({ result: "passed", consoleErrors, viewports: ["1672x941", "1024x768", "390x844"] }, null, 2));
} finally {
  await browser.close();
}
