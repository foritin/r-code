const fs = require('fs');
const path = require('path');
const { pathToFileURL } = require('url');
const { createRequire } = require('module');

const frontendRequire = createRequire(path.resolve(__dirname, '../../../src-tauri/frontend/package.json'));
const { chromium } = frontendRequire('playwright-core');

const root = __dirname;
const smoke = process.argv.includes('--smoke');
const visualOnly = process.argv.includes('--visual-only');
const interactionsOnly = process.argv.includes('--interactions-only');
const outputDir = path.resolve(root, '..', '..', '..', 'target', 'ui-demo');
const sourceUrl = pathToFileURL(path.join(root, 'index.html')).href;
const playwrightCache = path.join(process.env.LOCALAPPDATA || '', 'ms-playwright');
const cachedChromium = fs.existsSync(playwrightCache)
  ? fs.readdirSync(playwrightCache)
    .filter((entry) => /^chromium-\d+$/.test(entry))
    .sort((a, b) => Number(b.split('-')[1]) - Number(a.split('-')[1]))
    .flatMap((entry) => [
      path.join(playwrightCache, entry, 'chrome-win64', 'chrome.exe'),
      path.join(playwrightCache, entry, 'chrome-win', 'chrome.exe'),
    ])
    .find((candidate) => fs.existsSync(candidate))
  : undefined;
const browserExecutable = [
  cachedChromium,
  path.join(process.env.PROGRAMFILES || '', 'Google', 'Chrome', 'Application', 'chrome.exe'),
  path.join(process.env['PROGRAMFILES(X86)'] || '', 'Google', 'Chrome', 'Application', 'chrome.exe'),
  path.join(process.env.LOCALAPPDATA || '', 'Google', 'Chrome', 'Application', 'chrome.exe'),
  path.join(process.env.PROGRAMFILES || '', 'Microsoft', 'Edge', 'Application', 'msedge.exe'),
].find((candidate) => candidate && fs.existsSync(candidate));

const scenarios = [
  { name: 'home', query: 'scene=home', selector: '.scene-home' },
  { name: 'dashboard', query: 'scene=dashboard&project=r-code', selector: '.scene-dashboard' },
  { name: 'conversations', query: 'scene=conversations', selector: '.scene-conversations' },
  { name: 'activity', query: 'scene=activity', selector: '.scene-activity' },
  { name: 'inbox', query: 'scene=inbox', selector: '.scene-inbox' },
  { name: 'projects', query: 'scene=projects', selector: '.scene-projects' },
  { name: 'editor', query: 'scene=editor&project=r-code&file=src/main.rs', selector: '.scene-editor' },
  { name: 'room-launcher', query: 'state=launcher&task=queue', selector: '.scene-room', workbench: { kind: 'launcher', mode: 'docked' } },
  { name: 'room-run', query: 'state=run&task=queue', selector: '.scene-room', workbench: { kind: 'summary', mode: 'docked' } },
  { name: 'room-files', query: 'state=files&task=queue', selector: '.scene-room', workbench: { kind: 'files', mode: 'docked' } },
  { name: 'room-terminal', query: 'state=terminal&task=queue', selector: '.scene-room', workbench: { kind: 'terminal', mode: 'docked' } },
  { name: 'room-review', query: 'state=review&task=review', selector: '.scene-room', workbench: { kind: 'review', mode: 'docked', section: 'review' } },
  { name: 'room-review-collapsed', query: 'state=review-collapsed&task=review', selector: '.scene-room', workbench: { mode: 'collapsed' } },
  { name: 'settings-providers', query: 'scene=settings&settings=providers', selector: '.settings-layout' },
  { name: 'settings-preferences', query: 'scene=settings&settings=preferences', selector: '.settings-layout' },
  { name: 'settings-diagnostics', query: 'scene=settings&settings=diagnostics', selector: '.settings-layout' },
  { name: 'settings-codex', query: 'scene=settings&settings=codex', selector: '.settings-layout' },
];
const themes = ['light', 'dark'];
const viewports = [
  { width: 1600, height: 1000, name: 'desktop' },
  { width: 1180, height: 820, name: 'compact' },
  { width: 900, height: 760, name: 'narrow' },
];
const progressFile = path.join(outputDir, 'qa-progress.log');

function progress(message) {
  const line = `${new Date().toISOString()} ${message}\n`;
  fs.appendFileSync(progressFile, line);
  process.stdout.write(line);
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

async function waitReady(page, scenario) {
  await page.waitForFunction(() => window.__ready === true, undefined, { timeout: 10000 });
  await page.waitForSelector(scenario.selector, { state: 'visible' });
  await page.evaluate(() => document.fonts?.ready ?? true);
}

async function auditLayout(page, scenario, theme) {
  const audit = await page.evaluate(({ selector }) => {
    const app = document.querySelector('#app');
    const main = document.querySelector('#main-content');
    const fatal = document.querySelector('.fatal-screen, .error-boundary');
    const workbenchRoot = document.querySelector('[data-testid="workbench-root"]');
    const workbenchPanel = document.querySelector('[data-testid="workbench-panel"]');
    const reviewRail = document.querySelector('[data-testid="review-collapsed"]');
    const visibleControls = [...document.querySelectorAll('button, input, textarea, select, [tabindex]:not([tabindex="-1"])')]
      .filter((element) => {
        const style = getComputedStyle(element);
        const rect = element.getBoundingClientRect();
        return style.display !== 'none' && style.visibility !== 'hidden' && rect.width > 0 && rect.height > 0;
      });
    const outside = visibleControls
      .filter((element) => {
        const rect = element.getBoundingClientRect();
        return rect.bottom > 0 && rect.top < innerHeight && rect.right > 0 && rect.left < innerWidth;
      })
      .filter((element) => {
        const rect = element.getBoundingClientRect();
        return rect.left < -2 || rect.right > innerWidth + 2;
      })
      .map((element) => element.getAttribute('aria-label') || element.textContent?.trim().slice(0, 36) || element.tagName);
    return {
      ready: document.documentElement.dataset.demoReady,
      theme: document.documentElement.dataset.theme,
      app: Boolean(app),
      main: Boolean(main),
      scene: Boolean(document.querySelector(selector)),
      fatal: Boolean(fatal),
      workbench: workbenchRoot ? {
        kind: workbenchPanel?.dataset.workbenchKind ?? null,
        section: workbenchPanel?.dataset.workbenchSection ?? null,
        mode: workbenchRoot.dataset.workbenchMode ?? null,
        layout: workbenchRoot.dataset.workbenchLayout ?? null,
        panelCount: document.querySelectorAll('[data-testid="workbench-panel"]').length,
        railCount: document.querySelectorAll('[data-testid="review-collapsed"]').length,
      } : null,
      outside,
      overflow: [
        Math.max(0, document.documentElement.scrollWidth - innerWidth),
        Math.max(0, document.documentElement.scrollHeight - innerHeight),
      ],
    };
  }, { selector: scenario.selector });
  assert(audit.ready === 'true', `${scenario.name}: demo never became ready`);
  assert(audit.app && audit.main && audit.scene, `${scenario.name}: required product surface missing`);
  assert(!audit.fatal, `${scenario.name}: error boundary rendered`);
  assert(audit.theme === (theme === 'dark' ? 'obsidian' : 'studio-light'), `${scenario.name}: theme ${audit.theme}`);
  assert(audit.overflow.every((value) => value <= 2), `${scenario.name}: page overflow ${audit.overflow.join(',')}`);
  assert(audit.outside.length === 0, `${scenario.name}: controls outside viewport ${audit.outside.join(', ')}`);
  if (scenario.workbench) {
    assert(audit.workbench, `${scenario.name}: workbench root missing`);
    assert(audit.workbench.mode === scenario.workbench.mode, `${scenario.name}: workbench mode ${audit.workbench.mode}`);
    if (scenario.workbench.kind) assert(audit.workbench.kind === scenario.workbench.kind, `${scenario.name}: workbench kind ${audit.workbench.kind}`);
    if (scenario.workbench.section) assert(audit.workbench.section === scenario.workbench.section, `${scenario.name}: workbench section ${audit.workbench.section}`);
    const expectedLayout = page.viewportSize().width <= 759 ? 'full' : page.viewportSize().width <= 1359 ? 'overlay' : page.viewportSize().width < 1600 ? 'compact' : 'wide';
    assert(audit.workbench.layout === expectedLayout, `${scenario.name}: workbench layout ${audit.workbench.layout}`);
    if (scenario.workbench.mode === 'collapsed') {
      assert(audit.workbench.panelCount === 0 && audit.workbench.railCount === 1, `${scenario.name}: collapsed surface count`);
    } else {
      assert(audit.workbench.panelCount === 1 && audit.workbench.railCount === 0, `${scenario.name}: workbench surface count`);
    }
  }
}

async function runVisualMatrix(browser, errors) {
  const activeScenarios = smoke ? scenarios.slice(0, 1) : scenarios;
  const activeThemes = smoke ? themes.slice(0, 1) : themes;
  const activeViewports = smoke ? viewports.slice(0, 1) : viewports;
  for (const viewport of activeViewports) {
    const page = await browser.newPage({
      viewport: { width: viewport.width, height: viewport.height },
      deviceScaleFactor: 1,
      reducedMotion: 'reduce',
    });
    page.setDefaultTimeout(10000);
    page.on('pageerror', (error) => errors.push(String(error)));
    page.on('console', (message) => { if (message.type() === 'error') errors.push(message.text()); });
    for (const scenario of activeScenarios) {
      for (const theme of activeThemes) {
        progress(`audit ${scenario.name}/${theme}/${viewport.name}`);
        await page.goto(`${sourceUrl}?${scenario.query}&theme=${theme}&reset=1`, { waitUntil: 'load' });
        await waitReady(page, scenario);
        await auditLayout(page, scenario, theme);
        if (viewport.width === 1600) {
          await page.locator('#app').screenshot({
            path: path.join(outputDir, `${scenario.name}-${theme}.png`),
            animations: 'disabled',
          });
        } else if (['home', 'room-launcher', 'inbox', 'settings-preferences'].includes(scenario.name) && theme === 'light') {
          await page.locator('#app').screenshot({
            path: path.join(outputDir, `${scenario.name}-${theme}-${viewport.width}.png`),
            animations: 'disabled',
          });
        }
      }
    }
    await page.close();
  }
}

async function clickRail(page, label, expectedSelector) {
  const item = page.locator('.sidebar-nav-item').filter({ hasText: label });
  assert(await item.count() === 1, `rail item ${label} is not unique`);
  await item.click();
  await page.waitForSelector(expectedSelector, { state: 'visible' });
}

async function runInteractions(browser, errors) {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, reducedMotion: 'reduce' });
  page.setDefaultTimeout(10000);
  page.on('pageerror', (error) => errors.push(String(error)));
  page.on('console', (message) => { if (message.type() === 'error') errors.push(message.text()); });

  await page.goto(`${sourceUrl}?scene=home&theme=light&reset=1`, { waitUntil: 'load' });
  await waitReady(page, scenarios[0]);

  progress('interaction global navigation');
  await clickRail(page, '对话', '.scene-conversations');
  await clickRail(page, '待处理', '.scene-inbox');
  await clickRail(page, '活动', '.scene-activity');
  await clickRail(page, '项目文件', '.scene-editor');
  const settings = page.locator('.sidebar-footer-action').filter({ hasText: '设置' });
  assert(await settings.count() === 1, 'settings entry is not unique');
  await settings.click();
  await page.waitForSelector('.settings-layout', { state: 'visible' });

  progress('interaction search overlay');
  await page.locator('.sidebar-search').click();
  await page.waitForSelector('.ovl[role="dialog"]', { state: 'visible' });
  await page.getByLabel('搜索文件与内容').fill('error');
  await page.keyboard.press('Escape');
  assert(await page.locator('.ovl[role="dialog"]').count() === 0, 'search overlay did not close');
  assert(await page.locator('.sidebar-search').evaluate((element) => document.activeElement === element), 'search trigger did not regain focus');

  progress('interaction create conversation');
  await page.locator('.sidebar-brand').click();
  await page.waitForSelector('.scene-home', { state: 'visible' });
  await page.locator('.home-composer textarea').fill('把浏览器 Demo 的主流程补完整');
  const send = page.locator('.home-composer .send-button');
  assert(await send.isEnabled(), 'new conversation send button is disabled');
  await send.click();
  await page.waitForSelector('.scene-room', { state: 'visible' });
  assert((await page.locator('.timeline').innerText()).includes('完整会话回复'), 'new conversation did not receive demo reply');

  const workbenchRoot = page.getByTestId('workbench-root');
  progress('interaction launcher keyboard and reopen');
  assert(await workbenchRoot.getAttribute('data-workbench-mode') === 'hidden', 'new task should start with workbench hidden');
  await page.locator('.room-workbench-toggle').click();
  assert(await page.getByTestId('workbench-panel').getAttribute('data-workbench-kind') === 'launcher', 'room toggle did not restore launcher');
  const launcherRows = page.locator('.workbench-launcher-row');
  await launcherRows.first().focus();
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('Enter');
  assert(await page.getByTestId('workbench-panel').getAttribute('data-workbench-kind') === 'terminal', 'launcher keyboard did not open terminal');
  const selectedTerminalBefore = await page.locator('.term-row.sel').first().innerText();
  await page.getByRole('button', { name: '隐藏工作台' }).click();
  assert(await workbenchRoot.getAttribute('data-workbench-mode') === 'hidden', 'terminal hide did not close workbench');
  await page.locator('.room-workbench-toggle').click();
  assert((await page.locator('.term-row.sel').first().innerText()) === selectedTerminalBefore, 'terminal selection was not restored');

  progress('interaction files draft persistence');
  await page.locator('.sidebar-task').filter({ hasText: '修复任务队列并发问题' }).click();
  await page.locator('.room-workbench-toggle').click();
  await page.locator('.workbench-launcher-row').filter({ hasText: '文件' }).click();
  await page.locator('.files-tree-row[title="README.md"]').click();
  const filesEditor = page.locator('.files-textarea');
  await filesEditor.waitFor({ state: 'visible' });
  const originalDraft = await filesEditor.inputValue();
  await filesEditor.fill(`${originalDraft}\n任务 A 的未保存草稿`);
  await page.keyboard.press('Alt+2');
  await page.keyboard.press('Alt+3');
  assert((await page.locator('.files-textarea').inputValue()).includes('任务 A 的未保存草稿'), 'file draft did not survive tool switch');

  progress('interaction task isolation');
  await page.locator('.sidebar-task').filter({ hasText: '统一错误处理规范' }).click();
  assert(await workbenchRoot.getAttribute('data-workbench-mode') === 'hidden', 'new task inherited open workbench');
  await page.locator('.room-workbench-toggle').click();
  await page.locator('.workbench-launcher-row').filter({ hasText: '文件' }).click();
  await page.locator('.files-tree-row[title="README.md"]').click();
  await page.locator('.files-textarea').waitFor({ state: 'visible' });
  assert(!(await page.locator('.files-textarea').inputValue()).includes('任务 A 的未保存草稿'), 'task B inherited task A draft');
  await page.locator('.sidebar-task').filter({ hasText: '修复任务队列并发问题' }).click();
  await page.locator('.files-textarea').waitFor({ state: 'visible' });
  assert((await page.locator('.files-textarea').inputValue()).includes('任务 A 的未保存草稿'), 'returning task lost its draft');

  progress('interaction review collapse and selection');
  await page.locator('.sidebar-task').filter({ hasText: '统一错误处理规范' }).click();
  await page.getByRole('button', { name: '打开工具启动器' }).click();
  await page.locator('.workbench-launcher-row').filter({ hasText: '审核' }).click();
  const changeRows = page.locator('.chg-row');
  if (await changeRows.count() > 1) await changeRows.nth(1).click();
  const selectedChange = await page.locator('.chg-row.sel .chg-path').innerText();
  await page.getByRole('tab', { name: '验证与决策' }).click();
  await page.getByRole('tab', { name: /变更/ }).click();
  assert((await page.locator('.chg-row.sel .chg-path').innerText()) === selectedChange, 'review selection did not survive section switch');
  await page.getByTestId('workbench-close').click();
  assert(await workbenchRoot.getAttribute('data-workbench-mode') === 'collapsed', 'pending review did not collapse');
  await page.getByRole('button', { name: '展开审核工作台' }).click();
  assert((await page.locator('.chg-row.sel .chg-path').innerText()) === selectedChange, 'collapsed review did not restore selection');

  progress('interaction focus and stress');
  await page.getByRole('button', { name: '专注工作台' }).click();
  assert(await workbenchRoot.getAttribute('data-workbench-mode') === 'focus', 'focus mode not entered');
  await page.keyboard.press('Escape');
  assert(await workbenchRoot.getAttribute('data-workbench-mode') === 'docked', 'focus mode not exited');
  for (let index = 0; index < 30; index += 1) {
    await page.keyboard.press(index % 2 ? 'Alt+2' : 'Alt+3');
    assert(await page.getByTestId('workbench-panel').count() === 1, `switch ${index} created duplicate workbench`);
  }

  progress('interaction inbox permission');
  await clickRail(page, '待处理', '.scene-inbox');
  const permissionRow = page.locator('.inbox-row').filter({ hasText: '优化 Rust 编译性能' });
  assert(await permissionRow.count() === 1, 'permission row missing');
  await permissionRow.click();
  const allowOnce = page.getByRole('button', { name: '允许一次', exact: true });
  await allowOnce.click();
  assert((await page.locator('.inbox-count').innerText()).trim() === '1 项', 'permission decision did not update inbox');

  await page.close();
}

(async () => {
  fs.mkdirSync(outputDir, { recursive: true });
  fs.writeFileSync(progressFile, '');
  const errors = [];
  const browser = await chromium.launch({ headless: true, ...(browserExecutable ? { executablePath: browserExecutable } : {}) });
  try {
    if (!interactionsOnly) await runVisualMatrix(browser, errors);
    if (!smoke && !visualOnly) await runInteractions(browser, errors);
    if (errors.length) throw new Error(`browser errors:\n${errors.join('\n')}`);
    process.stdout.write(`QA passed; output: ${outputDir}\n`);
  } finally {
    await browser.close();
  }
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
