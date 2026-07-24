const app = document.querySelector('.app');
const projectTitle = document.querySelector('#project-title');
const projectPath = document.querySelector('#project-path');
const contextTitle = document.querySelector('#context-title');
const statusProject = document.querySelector('#status-project');
const primaryCode = document.querySelector('#primary-code');
const secondaryCode = document.querySelector('#secondary-code');
const editorStage = document.querySelector('.editor-stage');

const projectMeta = {
  fable5: { title: 'fable5', path: '/Users/edy/git/fable5', defaultFile: 'OPUS.md' },
  charter: { title: 'charter', path: '/Users/edy/git/charter', defaultFile: 'README.md' },
};

const opusLines = [
  '# 给 Opus 的系统指令 — 目标：90% 的 Fable 5 体验',
  '',
  '> 用法：内容整体拷进 `~/.claude/CLAUDE.md`（或用 Opus 的项目 CLAUDE.md）。',
  '> 来源：234 个真实会话中用户 200+ 次纠错的浓缩。每条规则都对应真实的历史失败。',
  '',
  '## 你在和谁协作',
  '',
  '* 中文沟通，常用**语音输入**；同音错字自动纠正后按本意执行，不要复读错字。',
  '* 身份：AI 应用工程师（Wizlynn，voice-agent 东南亚语音客服平台），业余高强度 Vibe-coding。',
  '* 判断方式：**看到成品才有判断力**。给他看 mock、样例、对比，不要给他抽象论述。',
  '* 已有资产：vibejet 的 AGENTS.md 是他最成熟的工程规范。',
  '',
  '## 十条常驻行为规则',
  '',
  '1. **不迎合，第一性原理**。他的想法有问题就直说哪里有问题、为什么。',
  '2. **禁止三个平庸方案**。发散时先亮维度轴，5–7 个方案，每个配“看得见”的场景。',
  '3. **完成 ≠ 真实证据**。声称完成前必须有真实的运行证明。',
  '4. **放权 ≠ 隐身**。长跑每完成一阶段落盘进度，决策点不打断但写下假设。',
  '5. **大白话优先**。冷术语第一次出现必须跟一句人话解释。',
  '6. **知识薄弱先联网**。潮流/竞品/审美/最新实践先检索再回答。',
  '7. **动词精确执行**。“写计划”就写计划；“讨论”就只讨论。',
  '8. **讨论类不加载流程 skill**。纯讨论/答疑/闲聊直接用智商回答。',
  '9. **并行有度**。能并行就开 subagent，但主任务不要过度工程化。',
  '10. **交付带使用引导 + 分支纪律**。最终交付必含“怎么用”。',
  '',
  '## 机器档案（macOS / Darwin 24.6）',
  '',
  '* 终端 Ghostty；包管理 Homebrew；Python 用 uv；Node 用 pnpm；Docker 走 OrbStack。',
  '* 网络：内网代理网关，外网/被墙走它，内网不走。',
  '* 声音提醒 hooks 已配；judgment-dojo 哨兵挂在全局。',
];

const readmeLines = [
  '# Charter',
  '',
  'A Session-first environment for trustworthy agentic software work.',
  '',
  '## Product model',
  '',
  '- Projects provide working context.',
  '- Sessions hold conversation, worktree, evidence and review.',
  '- Files, Search and Changes are contextual tools, not global navigation.',
];

const htmlLines = [
  '<!doctype html>',
  '<html lang="zh-CN">',
  '  <head>',
  '    <meta charset="UTF-8" />',
  '    <title>Moon rotation</title>',
  '  </head>',
  '  <body>',
  '    <main class="scene">',
  '      <canvas id="moon"></canvas>',
  '    </main>',
  '    <script type="module">',
  '      const rotationSpeed = 0.003;',
  '      function updateRotation() {',
  '        requestAnimationFrame(updateRotation);',
  '      }',
  '      updateRotation();',
  '    </script>',
  '  </body>',
  '</html>',
];

const markdownFiles = new Set(['OPUS.md', 'README.md', 'CLAUDE.md', 'example.md', 'USAGE-GUIDE.md']);

function escapeHtml(value) {
  return value.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;');
}

function highlight(line, file) {
  const safe = escapeHtml(line);
  if (markdownFiles.has(file)) {
    if (safe.startsWith('#')) return `<span class="token-heading">${safe}</span>`;
    if (safe.startsWith('&gt;')) return `<span class="token-quote">${safe}</span>`;
    return safe.replace(/\*\*(.+?)\*\*/g, '<span class="token-strong">**$1**</span>');
  }
  return safe
    .replace(/(&lt;\/?[a-z][^&]*?&gt;)/gi, '<span class="token-tag">$1</span>')
    .replace(/(&quot;.*?&quot;)/g, '<span class="token-string">$1</span>')
    .replace(/\b(const|function|class|return|import|from)\b/g, '<span class="token-heading">$1</span>');
}

function linesFor(file) {
  if (file === 'OPUS.md') return opusLines;
  if (file === 'README.md') return readmeLines;
  if (file.endsWith('.html') || file.endsWith('.mjs')) return htmlLines;
  if (file.endsWith('.json')) {
    return ['{', '  "name": "fable5",', '  "private": true,', '  "scripts": { "dev": "node server.mjs" }', '}'];
  }
  return ['# ' + file, '', 'Selected from the project context pane.'];
}

function renderCode(host, file) {
  host.innerHTML = linesFor(file)
    .map(
      (line, index) =>
        `<div class="code-line" data-line="${index + 1}"><span>${highlight(line, file)}</span></div>`,
    )
    .join('');
  host.setAttribute('aria-label', `${file} content`);
}

function openFile(file) {
  document.querySelectorAll('.tree-row.file').forEach((row) => {
    row.classList.toggle('active', row.dataset.file === file);
  });
  document.querySelectorAll('.editor-tab').forEach((tab) => {
    tab.classList.toggle('active', tab.dataset.tabFile === file);
  });
  renderCode(primaryCode, file);
  const existing = document.querySelector(`.editor-tab[data-tab-file="${CSS.escape(file)}"]`);
  if (!existing) {
    const tab = document.createElement('button');
    tab.className = 'editor-tab active';
    tab.dataset.tabFile = file;
    tab.innerHTML = `${file}<i class="ci ci-close"></i>`;
    document.querySelectorAll('.editor-tab').forEach((item) => item.classList.remove('active'));
    document.querySelector('.primary-pane .editor-tabs').append(tab);
  }
}

function setProject(project) {
  const meta = projectMeta[project];
  if (!meta) return;
  document.querySelectorAll('.project-card').forEach((card) => {
    card.classList.toggle('active', card.dataset.project === project);
  });
  document.querySelectorAll('.tree-set').forEach((tree) => {
    tree.classList.toggle('active', tree.dataset.treeProject === project);
  });
  projectTitle.textContent = meta.title;
  projectPath.textContent = meta.path;
  contextTitle.textContent = meta.title;
  statusProject.textContent = meta.title;
  openFile(meta.defaultFile);
  if (window.innerWidth <= 1180) app.classList.remove('projects-open');
}

function setTool(tool) {
  app.dataset.tool = tool;
  document.querySelectorAll('.tool-tabs [data-tool]').forEach((button) => {
    const selected = button.dataset.tool === tool;
    button.classList.toggle('active', selected);
    button.setAttribute('aria-selected', String(selected));
  });
  document.querySelectorAll('.context-view').forEach((panel) => {
    panel.classList.toggle('active', panel.dataset.panel === tool);
  });
  if (app.classList.contains('context-collapsed')) toggleContext();
}

function toggleContext() {
  const collapsed = app.classList.toggle('context-collapsed');
  const button = document.querySelector('#context-toggle');
  button.setAttribute('aria-pressed', String(!collapsed));
  button.querySelector('span').textContent = collapsed ? 'Show context' : 'Hide context';
  button.querySelector('.ci').className = `ci ${collapsed ? 'ci-sidebar' : 'ci-sidebar-off'}`;
}

document.querySelector('#projects-toggle').addEventListener('click', () => {
  const open = app.classList.toggle('projects-open');
  document.querySelector('#projects-toggle').setAttribute('aria-pressed', String(open));
});

document.querySelectorAll('.project-card').forEach((card) => {
  card.addEventListener('click', () => setProject(card.dataset.project));
});

document.querySelector('#project-search').addEventListener('input', (event) => {
  const query = event.target.value.trim().toLowerCase();
  const cards = [...document.querySelectorAll('.project-card')];
  cards.forEach((card) => {
    card.hidden = !card.dataset.search.toLowerCase().includes(query);
  });
  document.querySelector('.project-empty').hidden = cards.some((card) => !card.hidden);
});

document.querySelectorAll('.tool-tabs [data-tool]').forEach((button) => {
  button.addEventListener('click', () => setTool(button.dataset.tool));
});

document.querySelector('#context-toggle').addEventListener('click', toggleContext);

document.querySelector('#split-editor').addEventListener('click', () => {
  const split = editorStage.classList.toggle('split');
  document.querySelector('.secondary-pane').setAttribute('aria-hidden', String(!split));
  const button = document.querySelector('#split-editor');
  button.querySelector('span').textContent = split ? 'Join' : 'Split';
});

document.addEventListener('click', (event) => {
  const folder = event.target.closest('.tree-row.folder');
  if (folder) {
    const expanded = folder.getAttribute('aria-expanded') === 'true';
    folder.setAttribute('aria-expanded', String(!expanded));
    document.querySelector(`[data-folder-children="${folder.dataset.folder}"]`).hidden = expanded;
    return;
  }
  const fileTarget = event.target.closest('[data-file], [data-tab-file]');
  if (fileTarget) openFile(fileTarget.dataset.file ?? fileTarget.dataset.tabFile);
});

window.addEventListener('resize', () => {
  if (window.innerWidth <= 1180) app.classList.remove('projects-open');
});

if (window.innerWidth <= 1180) app.classList.remove('projects-open');
renderCode(primaryCode, 'OPUS.md');
renderCode(secondaryCode, 'moon-rotation.html');
