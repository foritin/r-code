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
  { name: 'settings-agents', query: 'scene=settings&settings=agents', selector: '.settings-layout' },
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
    const splitter = document.querySelector('.room-splitter');
    const conversation = document.querySelector('.convo');
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
        splitterLineWidth: splitter ? getComputedStyle(splitter, '::before').width : null,
        splitterHitWidth: splitter ? getComputedStyle(splitter).width : null,
        conversationBorderRight: conversation ? getComputedStyle(conversation).borderRightWidth : null,
        panelBorderLeft: workbenchPanel ? getComputedStyle(workbenchPanel).borderLeftWidth : null,
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
      assert(audit.workbench.splitterLineWidth === '1px', `${scenario.name}: visible splitter is ${audit.workbench.splitterLineWidth}`);
      assert(Number.parseFloat(audit.workbench.splitterHitWidth) >= 8, `${scenario.name}: splitter hit target is ${audit.workbench.splitterHitWidth}`);
      assert(audit.workbench.conversationBorderRight === '0px', `${scenario.name}: conversation keeps a duplicate divider`);
      assert(audit.workbench.panelBorderLeft === '0px', `${scenario.name}: workbench keeps a duplicate divider`);
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
  const homeComposerContract = await page.locator('.home-composer').evaluate((shell) => {
    const textarea = shell.querySelector('textarea');
    const send = shell.querySelector('.send-button');
    return {
      radius: getComputedStyle(shell).borderRadius,
      inputHeight: getComputedStyle(textarea).minHeight,
      sendHeight: getComputedStyle(send).height,
      sendBackground: getComputedStyle(send).backgroundColor,
    };
  });
  progress('interaction new-conversation reasoning and pasted image capability');
  const homeTextarea = page.locator('.home-composer textarea');
  await page.locator('.home-composer .model-config-trigger').click();
  let homeModelConfig = page.getByRole('dialog', { name: '模型与推理配置' });
  await homeModelConfig.waitFor({ state: 'visible' });
  assert((await homeModelConfig.innerText()).includes('推理强度'), 'new conversation reasoning depth control missing');
  await page.keyboard.press('Escape');

  await homeTextarea.evaluate((textarea) => {
    const binary = atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=');
    const pngHeader = Uint8Array.from(binary, (character) => character.charCodeAt(0));
    const transfer = new DataTransfer();
    transfer.items.add(new File([pngHeader], 'pasted-image.png', { type: 'image/png' }));
    textarea.dispatchEvent(new ClipboardEvent('paste', { clipboardData: transfer, bubbles: true }));
  });
  const pastedImage = page.locator('.home-composer .attachment-chip.kind-image');
  await pastedImage.waitFor({ state: 'visible' });
  assert(!(await pastedImage.evaluate((element) => element.classList.contains('is-unsupported'))), 'OpenAI image was marked unsupported');
  await pastedImage.locator('.attachment-thumbnail').click();
  await page.getByRole('dialog', { name: /预览图片/ }).waitFor({ state: 'visible' });
  await page.keyboard.press('Escape');

  await page.locator('.home-composer .model-config-trigger').click();
  homeModelConfig = page.getByRole('dialog', { name: '模型与推理配置' });
  await homeModelConfig.locator('.model-config-row').filter({ hasText: '模型' }).click();
  await homeModelConfig.locator('.menu-item').filter({ hasText: 'deepseek-v4-pro' }).click();
  await homeModelConfig.locator('.model-switch-confirm .accent').click();
  await page.waitForFunction(() => {
    const dialog = document.querySelector('[role="dialog"][aria-label="模型与推理配置"]');
    return dialog?.textContent?.includes('思考模式') && dialog?.textContent?.includes('推理强度');
  });
  await page.waitForFunction(() => document.querySelector('.home-composer .attachment-chip.kind-image')?.classList.contains('is-unsupported'));
  const unsupportedStyle = await pastedImage.locator('.attachment-label').evaluate((element) => getComputedStyle(element).textDecorationLine);
  assert(unsupportedStyle.includes('line-through'), 'unsupported image label is not struck through');
  assert(!(await page.locator('.home-composer .send-button').isEnabled()), 'unsupported image should block accidental send');
  await page.locator('.home-composer .composer-attachment-input').setInputFiles({
    name: 'context.md',
    mimeType: 'text/markdown',
    buffer: Buffer.from('# Context\nReadable text attachment.'),
  });
  const textAttachment = page.locator('.home-composer .attachment-chip.kind-text');
  await textAttachment.waitFor({ state: 'visible' });
  assert(!(await textAttachment.evaluate((element) => element.classList.contains('is-unsupported'))), 'text attachment inherited image capability state');
  await page.locator('#app').screenshot({
    path: path.join(outputDir, 'home-pasted-image-unsupported-light.png'),
    animations: 'disabled',
  });

  await page.keyboard.press('Escape');
  await page.locator('.home-composer .model-config-trigger').click();
  homeModelConfig = page.getByRole('dialog', { name: '模型与推理配置' });
  await homeModelConfig.waitFor({ state: 'visible' });
  await homeModelConfig.locator('.model-config-row').filter({ hasText: '模型' }).click();
  await homeModelConfig.locator('.menu-item').filter({ hasText: 'gpt-5.6-sol' }).click();
  await homeModelConfig.locator('.model-switch-confirm .accent').click();
  await page.waitForFunction(() => !document.querySelector('.home-composer .attachment-chip.kind-image')?.classList.contains('is-unsupported'));
  await page.keyboard.press('Escape');
  await page.locator('.home-composer .model-config-trigger').click();
  homeModelConfig = page.getByRole('dialog', { name: '模型与推理配置' });
  await homeModelConfig.waitFor({ state: 'visible' });
  await homeModelConfig.locator('.model-config-row').filter({ hasText: '推理强度' }).click();
  await homeModelConfig.locator('.menu-item').filter({ hasText: /^高$/ }).click();
  await page.waitForFunction(() => document.querySelector('.home-composer .model-config-trigger')?.textContent?.includes('高'));

  await page.locator('.home-composer textarea').fill('把浏览器 Demo 的主流程补完整');
  const send = page.locator('.home-composer .send-button');
  assert(await send.isEnabled(), 'new conversation send button is disabled');
  await send.click();
  await page.waitForSelector('.scene-room', { state: 'visible' });
  await page.waitForSelector('.message-attachment-summary', { state: 'visible' });
  assert(await page.locator('.message-attachment-item').count() === 2, 'timeline did not preserve both image and text attachment metadata');
  assert((await page.locator('.composer .model-config-trigger').innerText()).includes('高'), 'new-conversation reasoning choice did not reach the created task');
  assert((await page.locator('.timeline').innerText()).includes('完整会话回复'), 'new conversation did not receive demo reply');
  assert(
    await page.locator('.toast-title').filter({ hasText: '已结束：' }).count() === 0,
    'normal conversation completion should not show a toast'
  );
  await page.locator('.comp-box textarea').focus();
  const roomComposerContract = await page.locator('.comp-box').evaluate((shell) => {
    const textarea = shell.querySelector('textarea');
    const send = shell.querySelector('.send');
    return {
      radius: getComputedStyle(shell).borderRadius,
      inputHeight: getComputedStyle(textarea).minHeight,
      sendHeight: getComputedStyle(send).height,
      sendBackground: getComputedStyle(send).backgroundColor,
      textareaOutline: getComputedStyle(textarea).outlineStyle,
      sendText: send.textContent.trim(),
    };
  });
  assert(roomComposerContract.radius === homeComposerContract.radius, 'room composer radius drifted from home');
  assert(roomComposerContract.inputHeight === homeComposerContract.inputHeight, 'room composer input height drifted from home');
  assert(roomComposerContract.sendHeight === homeComposerContract.sendHeight, 'room composer send height drifted from home');
  assert(roomComposerContract.sendBackground === homeComposerContract.sendBackground, 'room composer send color drifted from home');
  assert(roomComposerContract.textareaOutline === 'none', 'room composer rendered an inner textarea focus ring');
  assert(roomComposerContract.sendText === '发送', 'room composer send label missing');

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
  await page.keyboard.press('Control+P');
  assert(await page.getByTestId('workbench-panel').getAttribute('data-workbench-kind') === 'files', 'Ctrl+P did not open task files');
  await page.keyboard.press('Control+Shift+G');
  assert(await page.getByTestId('workbench-panel').getAttribute('data-workbench-kind') === 'review', 'Ctrl+Shift+G did not open review');
  await page.keyboard.press('Control+Alt+S');
  assert(await page.getByTestId('workbench-panel').getAttribute('data-workbench-kind') === 'summary', 'Ctrl+Alt+S did not open run summary');
  await page.keyboard.press('Control+Backquote');
  assert(await page.getByTestId('workbench-panel').getAttribute('data-workbench-kind') === 'terminal', 'Ctrl+` did not open terminal');
  const headerGap = await page.locator('.workbench-head').evaluate((header) => {
    const tab = header.querySelector('.workbench-active-tab')?.getBoundingClientRect();
    const add = header.querySelector('.workbench-add-button')?.getBoundingClientRect();
    return tab && add ? add.left - tab.right : -1;
  });
  assert(headerGap >= 6, 'new extension action is visually glued to the active tab');
  await page.locator('.workbench-head').screenshot({
    path: path.join(outputDir, 'room-workbench-head-light.png'),
    animations: 'disabled',
  });

  progress('interaction files draft persistence');
  await page.locator('.sidebar-task').filter({ hasText: '修复任务队列并发问题' }).click();

  progress('interaction timeline progressive disclosure');
  const timeline = page.locator('.timeline');
  const timelineText = await timeline.innerText();
  for (const protocol of ['delegate_task', 'collect_subagents', 'subagent_lifecycle']) {
    assert(!timelineText.includes(protocol), `timeline exposed internal protocol ${protocol}`);
  }
  const inheritedTurn = timeline.locator('.timeline-turn').filter({ hasText: '编辑历史消息后' });
  const activeBranchTurn = timeline.locator('.timeline-turn').filter({ hasText: '梳理任务队列执行路径' });
  assert(await inheritedTurn.locator('.run-summary, .timeline-subagent-chip').count() === 0, 'active-branch runs leaked into inherited history');
  assert(await activeBranchTurn.locator('.run-summary').count() === 1, 'main run was not attached to the active branch turn');
  assert(await activeBranchTurn.locator('.timeline-subagent-chip').count() === 2, 'subagents were not attached to their delegating turn');
  assert(await timeline.locator('.timeline-subagent-chip').count() === 2, 'timeline subagent chips were not grouped');
  const commandGroup = timeline.locator('.timeline-activity-event.kind-command').filter({ hasText: '运行了多个命令' });
  assert(await commandGroup.count() === 1, 'timeline multi-command group missing');
  await commandGroup.locator('.timeline-activity-toggle').click();
  assert(await commandGroup.locator('.timeline-command-list .tcard').count() === 2, 'multi-command group did not reveal child commands');
  const firstCommand = commandGroup.locator('.timeline-command-list .tcard').first();
  await firstCommand.locator('.tcard-head').click();
  assert(await firstCommand.locator('.tcard-body').isVisible(), 'child command output did not expand');
  const completedSubagent = timeline.locator('.timeline-subagent-chip.status-completed').first();
  await completedSubagent.click();
  assert(await page.getByTestId('workbench-panel').getAttribute('data-workbench-kind') === 'subagent-detail', 'subagent chip did not open the dedicated detail page');
  assert(await page.getByTestId('subagent-detail').count() === 1, 'subagent detail view missing');
  assert(await page.locator('.subagent-page-status.status-completed').count() === 1, 'completed subagent status missing from detail header');
  assert(await page.locator('.subagent-page-header .subagent-avatar svg').count() === 1, 'subagent identity icon missing from detail header');
  assert(await page.locator('.subagent-runtime-log').count() === 1, 'subagent runtime telemetry disclosure missing');
  assert((await page.locator('.subagent-session-permission').innerText()).trim() === '只读', 'default subagent permission was not rendered as read-only');
  assert(await page.locator('.subagent-transcript-message').count() === 3, 'streamed Codex reply fragments were not coalesced into coherent transcript blocks');
  assert(await page.locator('.subagent-transcript-speaker').count() === 3, 'subagent identity was repeated for token fragments');
  const transcriptText = await page.locator('.subagent-transcript').innerText();
  assert(transcriptText.includes('我先沿共享状态的获取顺序做只读检查'), 'coalesced subagent response lost streamed text');
  const subagentToolGroup = page.locator('.subagent-tool-group');
  assert(await subagentToolGroup.count() === 1, 'consecutive subagent tools were not grouped');
  assert((await subagentToolGroup.locator('.subagent-tool-group-head').innerText()).includes('运行了 2 项操作'), 'subagent tool group count missing');
  assert((await subagentToolGroup.locator('.subagent-tool-group-head').innerText()).includes('1 项失败'), 'subagent tool failure summary missing');
  await subagentToolGroup.locator('.subagent-tool-group-head').click();
  assert(await page.locator('.subagent-transcript-tool').count() === 3, 'Codex tool activity was not rendered after group expansion');
  const completedTool = page.locator('.subagent-transcript-tool.state-ok').last();
  await completedTool.locator('.subagent-transcript-tool-head').click();
  assert(await completedTool.locator('.subagent-transcript-tool-body').isVisible(), 'Codex command output did not expand');
  assert((await completedTool.locator('.subagent-transcript-tool-body').innerText()).includes('8 passed'), 'real Codex command output was not exposed in the detail');
  assert((await page.locator('.subagent-session-state').innerText()).includes('1 项操作失败'), 'completed run concealed partial tool failures');
  await page.locator('#app').screenshot({
    path: path.join(outputDir, 'room-subagent-detail-light.png'),
    animations: 'disabled',
  });
  const subagentSession = page.locator('.subagent-session-summary');
  await subagentSession.click();
  assert(!(await page.locator('.subagent-session-body').isVisible()), 'subagent session did not collapse');
  await subagentSession.click();
  assert(await page.locator('.subagent-session-body').isVisible(), 'subagent session did not expand');
  await page.getByRole('button', { name: '返回子智能体列表' }).click();
  assert(await page.getByTestId('workbench-panel').getAttribute('data-workbench-kind') === 'subagents', 'subagent detail did not return to list');
  assert(await page.locator('.subagent-list-row').count() === 2, 'subagent list did not contain every run');
  assert(await page.locator('.subagent-list-section').count() === 2, 'subagent list did not separate active and completed runs');
  assert(
    JSON.stringify(await page.locator('.subagent-list-section > h3').allTextContents()) === JSON.stringify(['进行中 · 01', '已完成 · 01']),
    'subagent group counts are not adjacent to their labels',
  );
  assert(await page.locator('.subagent-list-row .subagent-spinner, .subagent-list-row .subagent-complete-mark').count() === 0, 'subagent rows duplicated status beside time');
  assert(/^\d+(?:m \d{2}s|s)$/.test((await page.locator('.subagent-list-row.status-running time').innerText()).trim()), 'running subagent did not show live elapsed time');
  assert(/^(?:刚刚|\d+(?:分钟|小时|天|个月|年)前)$/.test((await page.locator('.subagent-list-row.status-completed time').innerText()).trim()), 'completed subagent did not show relative completion time');
  assert(await page.getByTestId('subagent-tab-close').count() === 1, 'subagent workbench tab has no close action');
  assert(await page.getByRole('button', { name: '打开工具启动器' }).count() === 1, 'subagent workbench header has no extension action');
  assert(await page.getByRole('button', { name: '隐藏工作台' }).count() === 1, 'subagent workbench header has no hide action');
  await page.locator('#app').screenshot({
    path: path.join(outputDir, 'room-subagents-light.png'),
    animations: 'disabled',
  });
  const darkDetailPage = await browser.newPage({ viewport: { width: 1440, height: 900 }, reducedMotion: 'reduce' });
  darkDetailPage.setDefaultTimeout(10000);
  darkDetailPage.on('pageerror', (error) => errors.push(String(error)));
  darkDetailPage.on('console', (message) => { if (message.type() === 'error') errors.push(message.text()); });
  await darkDetailPage.goto(`${sourceUrl}?state=run&task=queue&theme=dark&reset=1`, { waitUntil: 'load' });
  await waitReady(darkDetailPage, scenarios.find((scenario) => scenario.name === 'room-run'));
  await darkDetailPage.locator('.sum-subagents-button').click();
  await darkDetailPage.locator('#app').screenshot({
    path: path.join(outputDir, 'room-subagents-dark.png'),
    animations: 'disabled',
  });
  await darkDetailPage.locator('.subagent-list-row.status-completed').click();
  await darkDetailPage.locator('.subagent-tool-group-head').click();
  await darkDetailPage.locator('.subagent-transcript-tool.state-ok').last().locator('.subagent-transcript-tool-head').click();
  await darkDetailPage.locator('#app').screenshot({
    path: path.join(outputDir, 'room-subagent-detail-dark.png'),
    animations: 'disabled',
  });
  await darkDetailPage.close();
  await page.locator('.subagent-list-row.status-running').click();
  await page.keyboard.press('Alt+3');
  assert(await page.getByTestId('workbench-panel').getAttribute('data-workbench-kind') === 'files', 'tool shortcut remained trapped behind subagent detail');
  assert(await page.getByTestId('subagent-detail').count() === 0, 'subagent detail still overrode the selected tool');
  await page.keyboard.press('Alt+1');
  const auditText = await page.locator('.audit-list').innerText();
  assert(!auditText.includes('delegate_task') && !auditText.includes('collect_subagents'), 'workbench audit duplicated subagent protocol tools');
  await page.locator('.sum-subagents-button').click();
  assert(await page.getByTestId('workbench-panel').getAttribute('data-workbench-kind') === 'subagents', 'summary subagent entry did not open the list');
  await page.getByTestId('subagent-tab-close').click();
  assert(await page.getByTestId('workbench-panel').getAttribute('data-workbench-kind') === 'summary', 'closing the subagent tab did not restore run summary');
  await page.getByRole('button', { name: '打开工具启动器' }).click();
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

  progress('interaction tab close fallback, launcher restore, and review collapse');
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
  assert(await workbenchRoot.getAttribute('data-workbench-mode') === 'docked', 'closing one of multiple tabs hid the workbench');
  assert(await page.getByTestId('workbench-panel').getAttribute('data-workbench-kind') === 'files', 'closing the active tab did not select its left neighbor');
  await page.getByTestId('workbench-close').click();
  assert(await workbenchRoot.getAttribute('data-workbench-mode') === 'hidden', 'closing the final tab did not hide the workbench');
  await page.locator('.room-workbench-toggle').click();
  assert(await page.getByTestId('workbench-panel').getAttribute('data-workbench-kind') === 'launcher', 'reopening an empty workbench revived a closed tool');
  await page.locator('.workbench-launcher-row').filter({ hasText: '审核' }).click();
  assert((await page.locator('.chg-row.sel .chg-path').innerText()) === selectedChange, 'reopened review did not restore its selection');
  await page.getByRole('button', { name: '隐藏工作台' }).click();
  assert(await workbenchRoot.getAttribute('data-workbench-mode') === 'collapsed', 'hiding a pending review did not collapse it');
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

  progress('interaction provider-specific model configuration');
  await page.goto(`${sourceUrl}?state=run&task=review&theme=dark&reset=1`, { waitUntil: 'load' });
  await waitReady(page, scenarios.find((scenario) => scenario.name === 'room-run'));
  await page.locator('.model-config-trigger').click();
  const providerConfig = page.getByRole('dialog', { name: '模型与推理配置' });
  await providerConfig.waitFor({ state: 'visible' });
  const providerConfigText = await providerConfig.innerText();
  assert(providerConfigText.includes('DeepSeek'), 'DeepSeek provider label missing from task config');
  assert(providerConfigText.includes('思考模式'), 'DeepSeek thinking control missing');
  assert(providerConfigText.includes('推理强度'), 'DeepSeek reasoning control missing');
  await providerConfig.locator('.model-config-row').filter({ hasText: '思考模式' }).click();
  await providerConfig.locator('.menu-item').filter({ hasText: '关闭' }).click();
  await page.waitForFunction(() => document.querySelector('.model-config-trigger')?.textContent?.includes('思考关闭'));

  progress('interaction Codex model configuration');
  await page.goto(`${sourceUrl}?state=run&task=complete&theme=dark&reset=1`, { waitUntil: 'load' });
  await waitReady(page, scenarios.find((scenario) => scenario.name === 'room-run'));
  await page.locator('.model-config-trigger').click();
  const codexConfig = page.getByRole('dialog', { name: 'Codex 模型与推理配置' });
  await codexConfig.waitFor({ state: 'visible' });
  await codexConfig.locator('.model-config-row').filter({ hasText: '输出详略' }).click();
  await codexConfig.locator('.menu-item').filter({ hasText: '详细' }).click();
  await page.waitForFunction(() => {
    const dialog = document.querySelector('[role="dialog"][aria-label="Codex 模型与推理配置"]');
    return dialog?.textContent?.includes('输出详略') && dialog?.textContent?.includes('详细');
  });

  progress('interaction Codex image preview and local file routing');
  await page.keyboard.press('Escape');
  const imageArtifact = page.locator('.md-image-artifact').first();
  await imageArtifact.waitFor({ state: 'visible' });
  const imageThumb = imageArtifact.locator('.md-image-thumb');
  await imageThumb.waitFor({ state: 'visible' });
  assert(
    await imageArtifact.getByRole('button', { name: '在文件管理器中显示' }).count() === 1,
    'external Codex image was not classified for the OS file manager',
  );
  await imageThumb.click();
  const imageDialog = page.getByRole('dialog', { name: /图片预览/ });
  await imageDialog.waitFor({ state: 'visible' });
  assert(
    await imageDialog.locator('.image-preview-stage img').evaluate((image) => image.complete && image.naturalWidth > 0),
    'Codex image preview did not decode',
  );
  assert(
    await imageDialog.evaluate((dialog) => {
      const rect = dialog.getBoundingClientRect();
      return rect.left >= 0 && rect.top >= 0 && rect.right <= innerWidth && rect.bottom <= innerHeight;
    }),
    'Codex image preview escaped the viewport',
  );
  await page.screenshot({
    path: path.join(outputDir, 'codex-image-preview-dark.png'),
    animations: 'disabled',
  });
  await page.keyboard.press('Escape');
  assert(await imageDialog.count() === 0, 'Codex image preview did not close with Escape');
  assert(await imageThumb.evaluate((element) => document.activeElement === element), 'image preview trigger did not regain focus');
  await imageArtifact.getByRole('button', { name: '在文件管理器中显示' }).click();
  assert(
    (await page.locator('html').getAttribute('data-demo-revealed-path'))?.endsWith('/.codex/generated_images/r-code-preview.png'),
    'external Codex image did not route to the OS file manager command',
  );

  await page.locator('.md-file-link').filter({ hasText: '打开实现文件' }).click();
  assert(await page.getByTestId('workbench-panel').getAttribute('data-workbench-kind') === 'files', 'project file link did not open the Files workbench');
  const selectedSource = page.locator('.files-tree-row.selected[title="src/main.rs"]');
  await selectedSource.waitFor({ state: 'visible' });
  assert(await page.locator('.files-textarea').evaluate((element) => document.activeElement === element), 'project file line navigation did not focus the editor');
  await page.locator('#app').screenshot({
    path: path.join(outputDir, 'codex-file-routing-dark.png'),
    animations: 'disabled',
  });

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
