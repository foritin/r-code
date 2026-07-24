const refs = {
  file: { label: 'src/theme.css · 当前文件 L42–44', lines: 3 },
  editor: { label: 'src/components/Button.tsx · 编辑器 L18–24', lines: 7 },
  search: { label: 'public/index.html · 搜索结果 L16–18', lines: 3 },
};
const attached = new Set(Object.keys(refs));
const shelf = document.getElementById('unified-context-shelf');
const summary = document.getElementById('ref-summary');
const input = document.getElementById('unified-composer-input');
const send = document.getElementById('unified-send-button');
const workingCopy = document.getElementById('unified-working-copy');
const toast = document.getElementById('unified-toast');
const sceneButtons = [...document.querySelectorAll('[data-scene]')];
let toastTimer;

function showToast(message) {
  toast.textContent = message;
  toast.classList.add('visible');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => toast.classList.remove('visible'), 1900);
}

function setScene(scene) {
  document.body.classList.remove('scene-file', 'scene-editor', 'scene-search');
  document.body.classList.add(`scene-${scene}`);
  sceneButtons.forEach((button) =>
    button.classList.toggle('active', button.dataset.scene === scene),
  );
  const searchTab = document.getElementById('search-tab');
  const fileTab = document.getElementById('file-tab');
  searchTab.classList.toggle('hidden', scene !== 'search');
  searchTab.classList.toggle('active', scene === 'search');
  fileTab.classList.toggle('active', scene !== 'search');
  document.getElementById('expand-label').textContent = scene === 'editor' ? 'Collapse' : 'Expand';
}

function renderRefs() {
  const cards = [...document.querySelectorAll('[data-ref]')];
  cards.forEach((card) => card.classList.toggle('hidden', !attached.has(card.dataset.ref)));
  document.querySelectorAll('[data-source-action]').forEach((action) => {
    const source = action.dataset.sourceAction;
    const on = attached.has(source);
    action.classList.toggle('attached', on);
    if (action.classList.contains('selection-action')) {
      const button = action.querySelector('.source-action-button');
      if (button) button.textContent = on ? '已在上下文' : '添加到上下文';
    } else {
      action.textContent = on ? '已在上下文' : '添加到上下文';
    }
  });
  const lineCount = [...attached].reduce((total, key) => total + refs[key].lines, 0);
  summary.textContent = `${attached.size} 个引用 · ${lineCount} 行`;
  shelf.classList.toggle('empty', attached.size === 0);
  workingCopy.textContent = attached.size
    ? `${attached.size} 个代码引用等待发送`
    : '等待你从右侧添加代码上下文…';
  send.disabled = attached.size === 0 || !input.value.trim();
}

function toggleRef(source) {
  if (attached.has(source)) {
    attached.delete(source);
    showToast(`已从草稿移除 ${refs[source].label}`);
  } else {
    attached.add(source);
    showToast(`已添加 ${refs[source].label}`);
    input.focus();
  }
  renderRefs();
}

sceneButtons.forEach((button) =>
  button.addEventListener('click', () => setScene(button.dataset.scene)),
);
document.querySelectorAll('[data-source-action]').forEach((action) => {
  action.addEventListener('click', () => toggleRef(action.dataset.sourceAction));
});
document.querySelectorAll('.remove-ref').forEach((button) => {
  button.addEventListener('click', (event) =>
    toggleRef(event.target.closest('[data-ref]').dataset.ref),
  );
});
document.querySelectorAll('[data-ref]').forEach((card) => {
  card.addEventListener('click', (event) => {
    if (event.target.closest('button')) return;
    card.classList.toggle('expanded');
  });
});
document.getElementById('reset-refs').addEventListener('click', () => {
  attached.clear();
  renderRefs();
  showToast('已清空草稿中的全部代码引用。');
});
document.getElementById('mock-search-button').addEventListener('click', () => {
  showToast('已刷新：8 个结果，来自当前 Session 的工作树。');
});
document
  .querySelector('.file-peek .quiet-button')
  .addEventListener('click', () => setScene('editor'));
document.getElementById('expand-label').addEventListener('click', () =>
  setScene(document.body.classList.contains('scene-editor') ? 'file' : 'editor'),
);
input.addEventListener('input', renderRefs);
send.addEventListener('click', () => {
  if (send.disabled) return;
  const sentEntry = document.getElementById('unified-sent-entry');
  document.getElementById('unified-sent-copy').textContent = input.value.trim();
  const refList = document.getElementById('unified-timeline-refs');
  refList.replaceChildren();
  [...attached].forEach((source) => {
    const row = document.createElement('span');
    row.textContent = refs[source].label;
    refList.append(row);
  });
  sentEntry.classList.remove('hidden');
  workingCopy.textContent = `Agent 正在读取 ${attached.size} 个精确代码引用…`;
  input.value = '';
  attached.clear();
  renderRefs();
  requestAnimationFrame(() => {
    const scroller = document.getElementById('unified-conversation-scroll');
    scroller.scrollTop = scroller.scrollHeight;
  });
  showToast('已发送；三个来源被固化成同一种时间线证据。');
});
setScene('file');
renderRefs();
