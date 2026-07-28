const fs = require('fs');
const path = require('path');
const { pathToFileURL } = require('url');
const { chromium } = require('playwright');

const root = __dirname;
const updatePrototypes = process.argv.includes('--update-prototypes');
const smoke = process.argv.includes('--smoke');
const visualOnly = process.argv.includes('--visual-only');
const interactionsOnly = process.argv.includes('--interactions-only');
const outputDir = updatePrototypes
  ? path.resolve(root, '..', 'prototypes', 'workbench')
  : path.resolve(root, '..', '..', '..', 'target', 'ui-demo');
const sourceUrl = pathToFileURL(path.join(root, 'index.html')).href;
const playwrightCache = path.join(process.env.LOCALAPPDATA || '', 'ms-playwright');
const localChromium = fs.existsSync(playwrightCache)
  ? fs.readdirSync(playwrightCache)
    .filter((entry) => /^chromium-\d+$/.test(entry))
    .sort((a, b) => Number(b.split('-')[1]) - Number(a.split('-')[1]))
    .flatMap((entry) => [
      path.join(playwrightCache, entry, 'chrome-win64', 'chrome.exe'),
      path.join(playwrightCache, entry, 'chrome-win', 'chrome.exe')
    ])
    .find((candidate) => fs.existsSync(candidate))
  : undefined;

const states = ['launcher', 'run', 'terminal', 'files', 'review', 'review-collapsed'];
const themes = ['light', 'dark'];
const viewports = [
  { width: 1600, height: 1000, layout: 'wide' },
  { width: 1440, height: 900, layout: 'compact' },
  { width: 1180, height: 820, layout: 'overlay' },
  { width: 1024, height: 768, layout: 'overlay' }
];
const prototypeNames = {
  'launcher-light': '01-launcher-light.png',
  'launcher-dark': '02-launcher-dark.png',
  'run-light': '03-subagents-light.png',
  'run-dark': '04-subagents-dark.png',
  'terminal-light': '05-terminal-light.png',
  'terminal-dark': '06-terminal-dark.png',
  'files-light': '07-files-light.png',
  'files-dark': '08-files-dark.png',
  'review-light': '09-review-light.png',
  'review-dark': '10-review-dark.png',
  'review-collapsed-light': '11-review-collapsed-light.png',
  'review-collapsed-dark': '12-review-collapsed-dark.png'
};
const progressFile = path.resolve(root, '..', '..', '..', 'target', 'ui-demo', 'qa-progress.log');
function progress(message) {
  const line = `${new Date().toISOString()} ${message}\n`;
  fs.appendFileSync(progressFile, line);
  process.stdout.write(line);
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

async function waitReady(page) {
  await page.waitForFunction(() => window.__ready === true);
  await page.evaluate(() => document.fonts.ready.then(() => true));
}

async function auditLayout(page, expected) {
  const audit = await page.evaluate(() => {
    const root = document.querySelector('[data-testid="workbench-root"]');
    const visibleButtons = [...document.querySelectorAll('button')].filter((button) => {
      const style = getComputedStyle(button);
      const rect = button.getBoundingClientRect();
      return style.display !== 'none' && style.visibility !== 'hidden' && rect.width > 0 && rect.height > 0;
    });
    const clipped = visibleButtons.filter((button) => button.scrollWidth > button.clientWidth + 1 || button.scrollHeight > button.clientHeight + 1).map((button) => ({
      label: button.getAttribute('aria-label') || button.textContent.trim().slice(0, 40),
      client: [button.clientWidth, button.clientHeight],
      scroll: [button.scrollWidth, button.scrollHeight]
    }));
    return {
      root: {
        kind: root?.dataset.workbenchKind,
        mode: root?.dataset.workbenchMode,
        layout: root?.dataset.workbenchLayout,
        theme: root?.dataset.demoTheme
      },
      pageOverflow: [document.documentElement.scrollWidth - innerWidth, document.documentElement.scrollHeight - innerHeight],
      panelCount: document.querySelectorAll('[data-testid="workbench-panel"]').length,
      railCount: document.querySelectorAll('[data-testid="review-collapsed"]').length,
      clipped
    };
  });
  assert(audit.root.layout === expected.layout, `layout ${audit.root.layout}, expected ${expected.layout}`);
  assert(audit.pageOverflow.every((value) => value <= 1), `page overflow ${JSON.stringify(audit.pageOverflow)}`);
  assert(audit.clipped.length === 0, `clipped buttons ${JSON.stringify(audit.clipped)}`);
  if (expected.state === 'review-collapsed') {
    assert(audit.panelCount === 0 && audit.railCount === 1, `collapsed counts ${JSON.stringify(audit)}`);
    assert(audit.root.mode === 'collapsed', `collapsed mode ${audit.root.mode}`);
  } else {
    assert(audit.panelCount === 1 && audit.railCount === 0, `panel counts ${JSON.stringify(audit)}`);
    assert(audit.root.mode === 'docked', `docked mode ${audit.root.mode}`);
  }
  assert(audit.root.theme === expected.theme, `theme ${audit.root.theme}, expected ${expected.theme}`);
}

async function runVisualMatrix(browser, errors) {
  for (const viewport of (smoke ? viewports.slice(0, 1) : viewports)) {
    const page = await browser.newPage({ viewport: { width: viewport.width, height: viewport.height }, deviceScaleFactor: 1, reducedMotion: 'reduce' });
    page.setDefaultTimeout(8000);
    page.on('pageerror', (error) => errors.push(String(error)));
    page.on('console', (message) => { if (message.type() === 'error') errors.push(message.text()); });
    for (const state of (smoke ? states.slice(0, 1) : states)) {
      for (const theme of (smoke ? themes.slice(0, 1) : themes)) {
        process.stdout.write(`audit ${state}/${theme}/${viewport.width}\n`);
        await page.goto(`${sourceUrl}?state=${state}&theme=${theme}`, { waitUntil: 'load', timeout: 10000 });
        await waitReady(page);
        await auditLayout(page, { ...viewport, state, theme });
        if (viewport.width === 1600) {
          const filename = prototypeNames[`${state}-${theme}`];
          await page.locator('#stage').screenshot({ path: path.join(outputDir, filename), animations: 'disabled' });
          process.stdout.write(`rendered ${filename}\n`);
        } else if (!updatePrototypes && state === 'review' && theme === 'light') {
          await page.locator('#stage').screenshot({ path: path.join(outputDir, `responsive-${viewport.width}-review-light.png`), animations: 'disabled' });
        }
      }
    }
    await page.close();
  }
}

async function runInteractions(browser, errors) {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, reducedMotion: 'reduce' });
  page.setDefaultTimeout(5000);
  page.on('pageerror', (error) => errors.push(String(error)));
  page.on('console', (message) => { if (message.type() === 'error') errors.push(message.text()); });
  await page.goto(sourceUrl, { waitUntil: 'load' });
  await waitReady(page);
  assert(await page.locator('html').getAttribute('data-theme') === 'light', 'fresh Demo did not default to light');
  await page.goto(`${sourceUrl}?state=launcher&theme=light`, { waitUntil: 'load' });
  await waitReady(page);
  const root = page.locator('[data-testid="workbench-root"]');

  progress('interaction subagents');
  await page.getByRole('option', { name: /运行与子代理/ }).click();
  assert(await root.getAttribute('data-workbench-kind') === 'subagents', 'launcher did not open subagents');
  await page.getByRole('button', { name: /交互结构/ }).click();
  await page.getByRole('button', { name: '隐藏工作台' }).click();
  assert(await root.getAttribute('data-workbench-mode') === 'closed', 'subagents hide did not close panel');
  await page.getByRole('button', { name: '打开工作台' }).click();
  assert(await root.getAttribute('data-workbench-kind') === 'subagents', 'reopen did not restore subagents');

  progress('interaction launcher keyboard');
  await page.getByRole('button', { name: '打开其他工具' }).click();
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('Enter');
  assert(await root.getAttribute('data-workbench-kind') === 'review', 'launcher keyboard did not open review');
  await page.getByRole('button', { name: /Workbench\.tsx/ }).first().click();
  await page.getByTestId('workbench-close').click();
  assert(await root.getAttribute('data-workbench-mode') === 'collapsed', 'pending review did not collapse');
  await page.getByRole('button', { name: '展开审核摘要' }).click();
  assert(await page.locator('.review-file-row.is-selected strong').textContent() === 'Workbench.tsx', 'review file was not restored');

  progress('interaction terminal');
  await page.keyboard.press('Control+`');
  await page.locator('#terminal-input').fill('pwd');
  await page.locator('#terminal-input').press('Enter');
  assert((await page.locator('.terminal-output').textContent()).includes('D:\\project\\rust\\r-code'), 'terminal output missing');
  await page.getByRole('button', { name: '隐藏工作台' }).click();
  await page.keyboard.press('Control+`');
  assert((await page.locator('.terminal-output').textContent()).includes('pwd'), 'hidden terminal lost output');
  await page.getByRole('button', { name: '结束终端' }).click();
  await page.getByRole('button', { name: '取消' }).click();
  assert((await page.locator('.tool-pill').textContent()).includes('已连接'), 'cancel ended terminal');
  await page.getByRole('button', { name: '结束终端' }).click();
  await page.getByRole('button', { name: '结束终端', exact: true }).last().click();
  assert((await page.locator('.tool-pill').textContent()).includes('已结束'), 'terminal confirm did not end session');

  progress('interaction files');
  await page.keyboard.press('Control+P');
  await page.getByRole('button', { name: /Composer\.tsx/ }).click();
  await page.keyboard.press('Control+`');
  await page.keyboard.press('Control+P');
  assert((await page.locator('.file-editor-title').textContent()).includes('Composer.tsx'), 'file selection did not survive switch');

  progress('interaction review and theme');
  await page.keyboard.press('Control+Shift+G');
  progress('review opened');
  await page.getByRole('button', { name: /接受变更/ }).click();
  progress('review confirm opened');
  await page.getByRole('button', { name: '取消' }).click();
  progress('review confirm cancelled');
  assert((await root.getAttribute('data-workbench-kind')) === 'review', 'cancel changed review state');

  const beforeThemeKind = await root.getAttribute('data-workbench-kind');
  await page.getByRole('button', { name: /切换为暗色|切换暗色/ }).click();
  progress('theme switched');
  assert(await page.locator('html').getAttribute('data-theme') === 'dark', 'theme did not switch to dark');
  assert(await root.getAttribute('data-workbench-kind') === beforeThemeKind, 'theme switch reset workbench');

  progress('interaction task isolation');
  await page.keyboard.press('Control+P');
  await page.getByRole('button', { name: /Workbench\.tsx/ }).click();
  await page.getByRole('button', { name: /梳理 API 风险/ }).click();
  assert(await root.getAttribute('data-workbench-mode') === 'closed', 'new task inherited open workbench');
  await page.keyboard.press('Control+P');
  assert((await page.locator('.file-editor-title').textContent()).includes('commands.rs'), 'new task inherited selected file');
  await page.getByRole('button', { name: /设计右侧工作台/ }).click();
  assert((await page.locator('.file-editor-title').textContent()).includes('Workbench.tsx'), 'returning task lost selected file');

  progress('interaction focus and stress');
  await page.getByRole('button', { name: '专注工作台' }).click();
  assert(await root.getAttribute('data-workbench-mode') === 'focus', 'focus mode not entered');
  await page.keyboard.press('Escape');
  assert(await root.getAttribute('data-workbench-mode') === 'docked', 'focus mode not exited');

  for (let index = 0; index < 30; index += 1) {
    await page.keyboard.press(index % 2 ? 'Control+P' : 'Control+`');
    assert(await page.getByTestId('workbench-panel').count() === 1, `switch ${index} created duplicate panel`);
  }
  await page.close();
}

(async () => {
  fs.mkdirSync(outputDir, { recursive: true });
  fs.mkdirSync(path.dirname(progressFile), { recursive: true });
  fs.writeFileSync(progressFile, '');
  const errors = [];
  const browser = await chromium.launch({ headless: true, ...(localChromium ? { executablePath: localChromium } : {}) });
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
