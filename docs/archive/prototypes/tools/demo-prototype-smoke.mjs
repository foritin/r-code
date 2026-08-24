/**
 * 原型 Demo 交互冒烟（不纳入 npm test —— 文件名刻意不带 .test.mjs）。
 * 直接用 file:// 打开 docs/archive/prototypes/room-redesign-c.html，
 * 把每个可点击区域都点一遍，断言状态变化并输出关键截图。
 *
 *   node docs/archive/prototypes/tools/demo-prototype-smoke.mjs
 */
import assert from "node:assert/strict";
import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "..");
const frontendDir = path.join(repoRoot, "src-tauri", "frontend");
const requireFromFrontend = createRequire(path.join(frontendDir, "package.json"));
const { chromium } = requireFromFrontend("playwright-core");
const demoPath = path.join(repoRoot, "docs", "archive", "prototypes", "room-redesign-c.html");
const shotDir = path.join(repoRoot, "docs", "archive", "prototypes", "screenshots", "demo-interactions");
fs.mkdirSync(shotDir, { recursive: true });

function browserExecutable() {
  const playwrightCache = path.join(process.env.LOCALAPPDATA ?? "", "ms-playwright");
  const cached = fs.existsSync(playwrightCache)
    ? fs.readdirSync(playwrightCache)
      .filter((entry) => /^chromium-\d+$/.test(entry))
      .sort((left, right) => Number(right.split("-")[1]) - Number(left.split("-")[1]))
      .flatMap((entry) => [path.join(playwrightCache, entry, "chrome-win64", "chrome.exe")])
      .find((candidate) => fs.existsSync(candidate))
    : undefined;
  return [
    cached,
    path.join(process.env.PROGRAMFILES ?? "", "Google", "Chrome", "Application", "chrome.exe"),
    path.join(process.env.PROGRAMFILES ?? "", "Microsoft", "Edge", "Application", "msedge.exe"),
  ].find((candidate) => candidate && fs.existsSync(candidate));
}

const browser = await chromium.launch({ executablePath: browserExecutable(), headless: true });
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
const consoleErrors = [];
page.on("console", (msg) => { if (msg.type() === "error") consoleErrors.push(msg.text()); });
page.on("pageerror", (err) => consoleErrors.push(String(err)));

await page.goto(`file:///${demoPath.replace(/\\/g, "/")}`);
await page.waitForSelector("#tabAgents.active");

let step = "初始状态";
try {
  // 1. 工作台标签切换
  step = "标签切换";
  await page.click("#tabRun");
  assert.equal(await page.isVisible("#wbPaneRun"), true, "运行面板应可见");
  await page.click("#tabAgents");
  assert.equal(await page.isVisible("#agentsList"), true, "目录列表应可见");

  // 2. 目录条目 → 详情
  step = "目录详情";
  await page.click('.ag-row[data-agent="explore-ui"]');
  assert.equal(await page.isVisible("#agentDetail"), true, "详情应可见");
  assert.match(await page.textContent("#agTitle"), /摸清主交互页 UI 现状/);
  assert.match(await page.textContent("#agChip"), /已完成/);
  await page.click("#agentBack");
  assert.equal(await page.isVisible("#agentsList"), true, "返回后列表应可见");

  // 3. 时间线子代理芯片 → 目录详情（先展开子代理行）
  step = "芯片联动";
  const subagentRow = page.locator("button.ev", { hasText: "子智能体" }).first();
  await subagentRow.click();
  await page.click('.sub-chip[data-open-agent="run-verify"]');
  assert.equal(await page.isVisible("#agentDetail"), true, "芯片应打开详情");
  assert.match(await page.textContent("#agTitle"), /验证原型并截图走查/);
  assert.match(await page.textContent("#agChip"), /运行中/);
  await page.screenshot({ path: path.join(shotDir, "01-agent-detail.png") });
  await page.click("#agentBack");

  // 4. 追问子代理
  step = "追问子代理";
  await page.click('.ag-row[data-agent="design-thesis"]');
  await page.fill("#agAskInput", "把结论整理成变更清单");
  await page.click("#agAskSend");
  assert.match(await page.textContent("#agTranscript"), /追问已排队/);
  await page.click("#agentBack");

  // 5. 待办卡展开
  step = "待办卡";
  await page.click(".todo-head");
  assert.equal(await page.locator(".todo-list li").count(), 5, "待办应有 5 项");
  assert.equal(await page.locator(".todo-list li.done").count(), 2);
  assert.equal(await page.locator(".todo-list li.cur").count(), 1);
  await page.screenshot({ path: path.join(shotDir, "02-todo-expanded.png") });

  // 6. 分段开关：分组视图
  step = "分段开关";
  await page.click('.seg-btn[data-seg="group"]');
  assert.equal(await page.isVisible("#groupView"), true);
  assert.equal(await page.isVisible("#projView"), false);
  await page.screenshot({ path: path.join(shotDir, "03-rail-grouped.png") });
  await page.click('.seg-btn[data-seg="proj"]');

  // 7. 项目折叠 + 任务切换
  step = "项目折叠";
  await page.click(".proj-head");
  assert.equal(await page.isVisible("#projView .proj .proj-tasks"), false, "项目任务应折叠");
  await page.click(".proj-head");
  step = "任务切换";
  await page.click('.task:has-text("统一错误处理规范")');
  assert.match(await page.textContent("#rhTitleTxt"), /统一错误处理规范/);
  await page.click('.task:has-text("主交互页重设计")');

  // 8. 添加附件（+ 菜单真实添加 chip）
  step = "添加附件";
  const before = await page.locator("#chipRow .attach-chip").count();
  await page.click('[data-menu-btn="add"]');
  await page.click("#miAddAttach");
  assert.equal(await page.locator("#chipRow .attach-chip").count(), before + 1, "应新增一个附件 chip");

  // 9. 运行中发送 → 进入队列
  step = "排队发送";
  await page.fill("#composerInput", "把截图也附到交付说明里");
  await page.press("#composerInput", "Enter");
  assert.equal(await page.isVisible("#queueBox"), true, "队列应出现");
  assert.match(await page.textContent("#queueList"), /把截图也附到交付说明里/);

  // 10. 权限批准
  step = "权限批准";
  await page.click("#permAllow");
  assert.equal(await page.isVisible("#spinRow"), true, "批准后应显示进行行");
  assert.match(await page.textContent("#threadCol"), /已允许执行 verify.py/);
  assert.match(await page.textContent("#wbPending"), /0/);
  await page.screenshot({ path: path.join(shotDir, "04-perm-resolved.png") });

  // 11. 中断运行
  step = "中断运行";
  await page.click("#composeAct");
  assert.match(await page.textContent("#runStatus"), /已停止/);
  assert.match(await page.textContent("#composeAct"), /发送/);

  // 12. 停止后再发送 → 新轮次
  step = "停止后发送";
  await page.fill("#composerInput", "继续");
  await page.click("#composeAct");
  const turns = await page.locator("#threadCol .turn").count();
  assert.ok(turns >= 3, "应追加新轮次");
  assert.match(await page.textContent("#rhState"), /正在执行/);

  // 13. Ctrl+N 新对话
  step = "新对话";
  await page.press("body", "Control+n");
  assert.match(await page.textContent("#threadCol"), /新对话已就绪/);
  assert.match(await page.textContent("#rhTitleTxt"), /新对话/);
  await page.screenshot({ path: path.join(shotDir, "05-new-chat.png") });

  // 14. 通知全部已读
  step = "通知已读";
  await page.click('[data-menu-btn="m-bell"]');
  await page.click("#bellReadAll");
  assert.equal(await page.locator(".tb-bell .bdg").count(), 0, "角标应消失");

  // 15. 侧栏收起 / 工作台显隐
  step = "布局开关";
  await page.click("#railToggle");
  assert.equal(await page.$eval(".rail", (el) => el.classList.contains("closed")), true);
  await page.click("#railToggle");
  await page.click("#wbToggle");
  assert.equal(await page.isVisible("#wb"), false);
  await page.click("#wbToggle");

  assert.deepEqual(consoleErrors, [], `console 不应有错误：${consoleErrors.join(" | ")}`);
  console.log("PASS: demo-prototype-smoke — 15 组交互全部通过，无 console 错误");
} catch (err) {
  await page.screenshot({ path: path.join(shotDir, `FAIL-${Date.now()}.png`) }).catch(() => {});
  console.error(`FAIL @ ${step}:`, err.message);
  process.exitCode = 1;
} finally {
  await browser.close();
}
