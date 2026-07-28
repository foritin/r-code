/* Charter · 方案 C 全窗拖放 + Context 页签 interactions. NOT product code. */
(function () {
  'use strict';
  const M = window.CharterMock;

  const veil = document.getElementById('dropveil');
  const targetDraft = document.getElementById('targetDraft');
  const targetPinned = document.getElementById('targetPinned');
  const shelf = document.getElementById('shelf');
  const shelfCount = document.getElementById('shelfCount');
  const draft = document.getElementById('draft');
  const draftList = document.getElementById('draftList');
  const pinnedList = document.getElementById('pinnedList');
  const toolPanel = document.getElementById('toolPanel');

  const draftItems = new Map();
  const pinnedItems = new Map();
  const INITIAL_PIN = {
    kind: 'file',
    name: 'STYLE_GUIDE.md',
    path: 'docs/STYLE_GUIDE.md',
    meta: '3.2 KB',
    source: '3 天前固定',
  };

  /* ------------------------------------------------------------ rendering */

  function ctxRow(item, scope) {
    const row = document.createElement('div');
    row.className = 'ctx-row is-' + item.kind;

    if (item.kind === 'image' && item.thumb) {
      const img = document.createElement('img');
      img.className = 'thumb';
      img.alt = '';
      img.src = item.thumb;
      row.appendChild(img);
    } else {
      const icon = document.createElement('span');
      icon.className = 'ci ' + M.iconFor(item);
      row.appendChild(icon);
    }

    const who = document.createElement('span');
    who.className = 'who';
    who.innerHTML =
      '<b></b><small></small>';
    who.querySelector('b').textContent = item.name;
    who.querySelector('small').textContent =
      (item.path || item.name) + (item.meta ? ' · ' + item.meta : '');
    row.appendChild(who);

    const badge = document.createElement('span');
    badge.className = 'src-badge';
    badge.textContent = item.source || '拖入';
    row.appendChild(badge);

    const act = document.createElement('button');
    act.type = 'button';
    act.className = 'act';
    act.textContent = scope === 'pinned' ? '解除固定' : '移除';
    act.addEventListener('click', () => {
      (scope === 'pinned' ? pinnedItems : draftItems).delete(item.path || item.name);
      refresh();
    });
    row.appendChild(act);
    return row;
  }

  function refresh() {
    /* Context tab lists */
    draftList.innerHTML = '';
    if (draftItems.size === 0) {
      draftList.innerHTML =
        '<div class="ctx-empty">还没有草稿引用 — 拖文件进窗口,或点 @ 引用。</div>';
    } else {
      for (const item of draftItems.values()) draftList.appendChild(ctxRow(item, 'draft'));
    }
    pinnedList.innerHTML = '';
    if (pinnedItems.size === 0) {
      pinnedList.innerHTML = '<div class="ctx-empty">没有固定项。</div>';
    } else {
      for (const item of pinnedItems.values()) pinnedList.appendChild(ctxRow(item, 'pinned'));
    }
    document.getElementById('draftGroupCount').textContent = draftItems.size + ' 项';
    document.getElementById('pinnedGroupCount').textContent = pinnedItems.size + ' 项';

    /* composer shelf mirrors draft refs */
    shelf.querySelectorAll('.ref-chip').forEach((chip) => chip.remove());
    for (const [key, item] of draftItems) {
      shelf.appendChild(
        M.chipNode(item, () => {
          draftItems.delete(key);
          refresh();
        })
      );
    }
    shelf.classList.toggle('visible', draftItems.size > 0);
    shelfCount.textContent = draftItems.size + ' 项引用 · 随这条消息一起发送';
    draft.placeholder = draftItems.size
      ? '补充说明(可选)— 引用将随这条消息一起提交…'
      : 'Follow up — starts a new Session in this project…';

    const total = draftItems.size + pinnedItems.size;
    document.getElementById('ctxTabCount').textContent = total;
    document.getElementById('ctxPillCount').textContent = total;
  }

  function addItems(items, scope) {
    const bucket = scope === 'pinned' ? pinnedItems : draftItems;
    let added = 0;
    for (const item of items) {
      const key = item.path || item.name;
      if (bucket.has(key)) continue;
      bucket.set(key, { source: '拖入', ...item });
      added += 1;
    }
    refresh();
    if (added) {
      M.toast(
        scope === 'pinned'
          ? added + ' 项已固定为 fable5 的项目上下文'
          : added + ' 项已加入这条消息的上下文'
      );
    }
  }

  /* --------------------------------------------------------- drop overlay */

  let depth = 0;

  function showVeil() {
    veil.classList.remove('hidden');
  }

  function hideVeil() {
    veil.classList.add('hidden');
    targetDraft.classList.remove('armed');
    targetPinned.classList.remove('armed');
    depth = 0;
  }

  document.addEventListener('dragenter', (event) => {
    if (!Array.from(event.dataTransfer.types || []).includes('Files')) return;
    event.preventDefault();
    depth += 1;
    showVeil();
  });

  document.addEventListener('dragover', (event) => {
    if (!Array.from(event.dataTransfer.types || []).includes('Files')) return;
    event.preventDefault();
  });

  document.addEventListener('dragleave', () => {
    depth = Math.max(0, depth - 1);
    if (depth === 0) hideVeil();
  });

  document.addEventListener('drop', (event) => {
    event.preventDefault();
    hideVeil();
  });

  for (const [card, scope] of [
    [targetDraft, 'draft'],
    [targetPinned, 'pinned'],
  ]) {
    card.addEventListener('dragenter', () => card.classList.add('armed'));
    card.addEventListener('dragleave', () => card.classList.remove('armed'));
    card.addEventListener('dragover', (event) => {
      event.preventDefault();
      event.dataTransfer.dropEffect = 'copy';
    });
    card.addEventListener('drop', async (event) => {
      event.preventDefault();
      event.stopPropagation();
      const items = await M.readDataTransfer(event.dataTransfer);
      hideVeil();
      if (items.length) addItems(items, scope);
    });
  }

  /* ------------------------------------------------------------------ tabs */

  const tabs = document.querySelectorAll('.tool-tabs button');

  function showTab(which) {
    tabs.forEach((tab) => tab.classList.toggle('active', tab.dataset.tab === which));
    document.getElementById('tabSummary').classList.toggle('tab-hidden', which !== 'summary');
    document.getElementById('tabContext').classList.toggle('tab-hidden', which !== 'context');
  }

  tabs.forEach((tab) => {
    tab.addEventListener('click', () => {
      if (!tab.dataset.tab) {
        M.toast('Mock:这个页签在本方案中不变,略过');
        return;
      }
      showTab(tab.dataset.tab);
    });
  });

  document.getElementById('ctxPill').addEventListener('click', () => {
    showTab('context');
    toolPanel.classList.remove('flash');
    void toolPanel.offsetWidth;
    toolPanel.classList.add('flash');
  });

  document.getElementById('shelfClear').addEventListener('click', () => {
    draftItems.clear();
    refresh();
  });

  document.getElementById('sendButton').addEventListener('click', () => {
    M.toast('Mock:发送后草稿引用随消息进入时间线;固定上下文保持不动');
  });

  /* ---------------------------------------------------------------- scenes */

  document.querySelectorAll('.mock-controls [data-act]').forEach((button) => {
    button.addEventListener('click', () => {
      const act = button.dataset.act;
      if (act === 'veil') {
        showVeil();
        setTimeout(() => {
          targetDraft.classList.add('armed');
          setTimeout(() => hideVeil(), 1400);
        }, 500);
      }
      if (act === 'demo') {
        showVeil();
        setTimeout(() => targetDraft.classList.add('armed'), 250);
        setTimeout(() => {
          addItems([{ ...M.DEMO_ITEMS.screenshot, thumb: M.DEMO_ITEMS.screenshot.thumb }], 'draft');
          targetDraft.classList.remove('armed');
          targetPinned.classList.add('armed');
        }, 900);
        setTimeout(() => {
          addItems([{ ...M.DEMO_ITEMS.folder }], 'pinned');
          hideVeil();
          showTab('context');
        }, 1600);
      }
      if (act === 'reset') {
        draftItems.clear();
        pinnedItems.clear();
        pinnedItems.set(INITIAL_PIN.path, { ...INITIAL_PIN });
        draft.value = '';
        hideVeil();
        showTab('context');
        refresh();
      }
    });
  });

  pinnedItems.set(INITIAL_PIN.path, { ...INITIAL_PIN });
  showTab('context');
  refresh();
})();
