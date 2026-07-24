/* Charter · 方案 A 文件抽屉 interactions. NOT product code. */
(function () {
  'use strict';
  const M = window.CharterMock;

  const TREE = [
    {
      kind: 'folder',
      name: 'public',
      open: true,
      children: [
        { kind: 'file', name: 'index.html', changed: '+5 −2' },
        { kind: 'file', name: 'favicon.svg' },
        {
          kind: 'folder',
          name: 'styles',
          children: [
            { kind: 'file', name: 'base.css' },
            { kind: 'file', name: 'checkout.css' },
          ],
        },
      ],
    },
    {
      kind: 'folder',
      name: 'src',
      open: true,
      children: [
        {
          kind: 'folder',
          name: 'cart',
          children: [
            { kind: 'file', name: 'checkout.ts' },
            { kind: 'file', name: 'coupon.ts' },
          ],
        },
        { kind: 'file', name: 'main.ts' },
      ],
    },
    {
      kind: 'folder',
      name: 'assets',
      children: [
        { kind: 'image', name: 'banner.png' },
        { kind: 'image', name: 'coupon-expired@2x.png' },
      ],
    },
    {
      kind: 'folder',
      name: 'docs',
      children: [{ kind: 'file', name: 'STYLE_GUIDE.md' }],
    },
    { kind: 'file', name: 'package.json' },
    { kind: 'file', name: 'README.md' },
  ];

  const popover = document.getElementById('attachPopover');
  const scroll = document.getElementById('drawerScroll');
  const search = document.getElementById('drawerSearch');
  const composer = document.getElementById('composer');
  const shelf = document.getElementById('shelf');
  const shelfCount = document.getElementById('shelfCount');
  const draft = document.getElementById('draft');
  const attached = new Map();

  function countDescendants(node) {
    if (!node.children) return 0;
    return node.children.reduce((sum, child) => sum + 1 + countDescendants(child), 0);
  }

  function toItem(node, path) {
    return {
      kind: node.kind,
      name: node.name,
      path,
      meta: node.kind === 'folder' ? countDescendants(node) + ' 项' : node.changed || '',
      thumb: node.kind === 'image' ? M.tintedThumb('#a46220') : undefined,
    };
  }

  /* ------------------------------------------------------------ tree render */

  function renderRow(node, path) {
    const row = document.createElement('div');
    row.className = 'tree-row kind-' + node.kind + (node.open ? ' open' : '');
    row.draggable = true;
    row.dataset.path = path;

    const twist = document.createElement('span');
    twist.className = 'twist';
    if (node.kind === 'folder') twist.innerHTML = '<span class="ci ci-chevron-right"></span>';
    row.appendChild(twist);

    const icon = document.createElement('span');
    icon.className =
      'ci ' + (node.kind === 'folder' ? 'ci-folder' : M.iconFor({ kind: node.kind, name: node.name }));
    row.appendChild(icon);

    const label = document.createElement('span');
    label.className = 'label';
    label.textContent = node.name;
    label.title = path;
    row.appendChild(label);

    if (node.changed) {
      const badge = document.createElement('span');
      badge.className = 'badge-changed';
      badge.textContent = node.changed;
      row.appendChild(badge);
    } else if (node.kind === 'folder') {
      const hint = document.createElement('span');
      hint.className = 'hint';
      hint.textContent = countDescendants(node) + ' 项';
      row.appendChild(hint);
    }

    const add = document.createElement('button');
    add.type = 'button';
    add.className = 'row-add';
    add.title = '加入上下文';
    add.innerHTML = '<span class="ci ci-add"></span>';
    add.addEventListener('click', (event) => {
      event.stopPropagation();
      addToShelf(toItem(node, path));
    });
    row.appendChild(add);

    row.addEventListener('dragstart', (event) => {
      M.setRefPayload(event, toItem(node, path));
      row.classList.add('dragging');
    });
    row.addEventListener('dragend', () => row.classList.remove('dragging'));
    return row;
  }

  function renderTree(nodes, parentPath, container) {
    for (const node of nodes) {
      const path = parentPath ? parentPath + '/' + node.name : node.name;
      const row = renderRow(node, path);
      container.appendChild(row);
      if (node.kind === 'folder' && node.children) {
        const childBox = document.createElement('div');
        childBox.className = 'tree-children' + (node.open ? '' : ' collapsed');
        renderTree(node.children, path, childBox);
        container.appendChild(childBox);
        row.addEventListener('click', () => {
          node.open = !node.open;
          row.classList.toggle('open', node.open);
          childBox.classList.toggle('collapsed', !node.open);
        });
      }
    }
  }

  function flatten(nodes, parentPath, out) {
    for (const node of nodes) {
      const path = parentPath ? parentPath + '/' + node.name : node.name;
      out.push({ node, path });
      if (node.children) flatten(node.children, path, out);
    }
    return out;
  }

  function renderDrawer(query) {
    scroll.innerHTML = '';
    if (query) {
      const matches = flatten(TREE, '', []).filter(({ path }) =>
        path.toLowerCase().includes(query.toLowerCase())
      );
      const section = document.createElement('div');
      section.className = 'tree-section';
      section.textContent = '搜索结果 · ' + matches.length;
      scroll.appendChild(section);
      const box = document.createElement('div');
      box.className = 'file-tree';
      for (const { node, path } of matches) {
        const row = renderRow(node, path);
        row.querySelector('.twist').innerHTML = '';
        const hint = document.createElement('span');
        hint.className = 'hint';
        hint.textContent = path;
        row.insertBefore(hint, row.querySelector('.row-add'));
        box.appendChild(row);
      }
      if (!matches.length) {
        box.innerHTML = '<div class="popover-empty">没有匹配的文件</div>';
      }
      scroll.appendChild(box);
      return;
    }

    const changedLabel = document.createElement('div');
    changedLabel.className = 'tree-section';
    changedLabel.textContent = '本次会话改动';
    scroll.appendChild(changedLabel);
    const changedBox = document.createElement('div');
    changedBox.className = 'file-tree';
    const changedNode = TREE[0].children[0];
    changedBox.appendChild(renderRow(changedNode, 'public/index.html'));
    scroll.appendChild(changedBox);

    const filesLabel = document.createElement('div');
    filesLabel.className = 'tree-section';
    filesLabel.textContent = '项目文件';
    scroll.appendChild(filesLabel);
    const treeBox = document.createElement('div');
    treeBox.className = 'file-tree';
    renderTree(TREE, '', treeBox);
    scroll.appendChild(treeBox);
  }

  /* ---------------------------------------------------------------- drawer */

  function openDrawer() {
    popover.classList.remove('hidden');
    renderDrawer('');
    search.value = '';
    search.focus();
    document.getElementById('drawerButton').classList.add('open');
  }

  function closeDrawer() {
    popover.classList.add('hidden');
    document.getElementById('drawerButton').classList.remove('open');
  }

  document.getElementById('drawerButton').addEventListener('click', () => {
    popover.classList.contains('hidden') ? openDrawer() : closeDrawer();
  });
  document.getElementById('mentionButton').addEventListener('click', () => {
    openDrawer();
    M.toast('Mock:@ 与 📎 共用同一个抽屉,@ 走键盘、📎 走浏览/拖拽');
  });
  search.addEventListener('input', () => renderDrawer(search.value.trim()));
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') closeDrawer();
  });
  document.addEventListener('pointerdown', (event) => {
    if (popover.classList.contains('hidden')) return;
    if (popover.contains(event.target)) return;
    if (event.target.closest('#drawerButton, #mentionButton')) return;
    closeDrawer();
  });

  /* ----------------------------------------------------------------- shelf */

  function refreshShelf() {
    shelf.classList.toggle('visible', attached.size > 0);
    shelfCount.textContent = attached.size + ' 项引用 · 随这条消息一起发送';
    draft.placeholder = attached.size
      ? '补充说明(可选)— 引用将随这条消息一起提交…'
      : 'Follow up — starts a new Session in this project…';
    document.querySelectorAll('.tree-row').forEach((row) => {
      row.classList.toggle('added', attached.has(row.dataset.path));
    });
  }

  function addToShelf(item) {
    const key = item.path || item.name;
    if (attached.has(key)) {
      M.toast('「' + item.name + '」已经在上下文里了');
      return;
    }
    attached.set(key, item);
    shelf.appendChild(
      M.chipNode(item, () => {
        attached.delete(key);
        refreshShelf();
      })
    );
    refreshShelf();
    M.toast(
      (item.kind === 'folder' ? '目录' : item.kind === 'image' ? '图片' : '文件') +
        '「' +
        item.name +
        '」已加入上下文'
    );
  }

  document.getElementById('shelfClear').addEventListener('click', () => {
    attached.clear();
    shelf.querySelectorAll('.ref-chip').forEach((chip) => chip.remove());
    refreshShelf();
  });

  M.bindDropZone(composer, {
    onItems: (items) => items.forEach(addToShelf),
  });

  document.getElementById('sendButton').addEventListener('click', () => {
    M.toast('Mock:发送后,引用会像正文一样进入时间线记录(参照 CodeContextRef 的已确认样式)');
  });

  /* ---------------------------------------------------------------- scenes */

  document.querySelectorAll('.mock-controls [data-act]').forEach((button) => {
    button.addEventListener('click', () => {
      const act = button.dataset.act;
      if (act === 'open-drawer') openDrawer();
      if (act === 'demo') {
        addToShelf({ ...M.DEMO_ITEMS.style });
        addToShelf({ ...M.DEMO_ITEMS.folder });
        addToShelf({ ...M.DEMO_ITEMS.screenshot, thumb: M.DEMO_ITEMS.screenshot.thumb });
      }
      if (act === 'reset') {
        attached.clear();
        shelf.querySelectorAll('.ref-chip').forEach((chip) => chip.remove());
        refreshShelf();
        closeDrawer();
        draft.value = '';
      }
    });
  });

  refreshShelf();
})();
