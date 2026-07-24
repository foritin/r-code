/* Charter · 方案 D @ 内联引用 interactions. NOT product code. */
(function () {
  'use strict';
  const M = window.CharterMock;

  const OPTIONS = [
    { kind: 'file', name: 'index.html', path: 'public/index.html', group: '建议', changed: '+5 −2' },
    { kind: 'file', name: 'checkout.ts', path: 'src/cart/checkout.ts', group: '文件' },
    { kind: 'file', name: 'coupon.ts', path: 'src/cart/coupon.ts', group: '文件' },
    { kind: 'file', name: 'main.ts', path: 'src/main.ts', group: '文件' },
    { kind: 'file', name: 'base.css', path: 'public/styles/base.css', group: '文件' },
    { kind: 'file', name: 'checkout.css', path: 'public/styles/checkout.css', group: '文件' },
    { kind: 'file', name: 'STYLE_GUIDE.md', path: 'docs/STYLE_GUIDE.md', group: '文件' },
    { kind: 'file', name: 'package.json', path: 'package.json', group: '文件' },
    { kind: 'folder', name: 'public', path: 'public', group: '文件夹', meta: '6 项' },
    { kind: 'folder', name: 'styles', path: 'public/styles', group: '文件夹', meta: '2 项' },
    { kind: 'folder', name: 'cart', path: 'src/cart', group: '文件夹', meta: '2 项' },
    { kind: 'folder', name: 'assets', path: 'assets', group: '文件夹', meta: '2 项' },
    { kind: 'image', name: 'banner.png', path: 'assets/banner.png', group: '图片' },
    { kind: 'image', name: 'coupon-expired@2x.png', path: 'assets/coupon-expired@2x.png', group: '图片' },
  ];

  const menu = document.getElementById('mentionMenu');
  const menuScroll = document.getElementById('menuScroll');
  const menuQuery = document.getElementById('menuQuery');
  const input = document.getElementById('draft');
  const composer = document.getElementById('composer');

  let open = false;
  let query = '';
  let selected = 0;
  let savedRange = null;
  let filtered = [];

  /* ------------------------------------------------------------- tokens */

  function tokenNode(item) {
    const token = document.createElement('span');
    token.className = 'inline-ref is-' + item.kind;
    token.contentEditable = 'false';
    token.title = item.path || item.name;

    if (item.kind === 'image' && item.thumb) {
      const img = document.createElement('img');
      img.alt = '';
      img.src = item.thumb;
      token.appendChild(img);
    } else {
      const icon = document.createElement('span');
      icon.className = 'ci ' + (item.kind === 'folder' ? 'ci-folder' : M.iconFor(item));
      token.appendChild(icon);
    }

    token.appendChild(document.createTextNode(item.path || item.name));

    const x = document.createElement('span');
    x.className = 'x';
    x.title = '移除引用';
    x.innerHTML = '<span class="ci ci-close"></span>';
    x.addEventListener('click', (event) => {
      event.preventDefault();
      event.stopPropagation();
      token.remove();
      input.focus();
    });
    token.appendChild(x);
    return token;
  }

  function insertToken(item, range) {
    input.focus();
    const target = range || caretRangeAtEnd();
    target.deleteContents();
    const token = tokenNode(item);
    target.insertNode(token);
    const space = document.createTextNode(' ');
    token.after(space);
    const sel = window.getSelection();
    const after = document.createRange();
    after.setStartAfter(space);
    after.collapse(true);
    sel.removeAllRanges();
    sel.addRange(after);
  }

  function caretRangeAtEnd() {
    const range = document.createRange();
    range.selectNodeContents(input);
    range.collapse(false);
    return range;
  }

  /* --------------------------------------------------------------- menu */

  function renderMenu() {
    filtered = OPTIONS.filter((option) =>
      (option.path + ' ' + option.name).toLowerCase().includes(query.toLowerCase())
    );
    selected = Math.min(selected, Math.max(0, filtered.length - 1));
    menuQuery.textContent = query;
    menuScroll.innerHTML = '';

    if (!filtered.length) {
      menuScroll.innerHTML = '<div class="menu-empty">没有匹配「' + query + '」的文件</div>';
      return;
    }

    let lastGroup = '';
    filtered.forEach((option, index) => {
      if (option.group !== lastGroup) {
        lastGroup = option.group;
        const label = document.createElement('div');
        label.className = 'menu-group';
        label.textContent = option.group;
        menuScroll.appendChild(label);
      }
      const row = document.createElement('div');
      row.className = 'menu-option kind-' + option.kind + (index === selected ? ' selected' : '');

      const icon = document.createElement('span');
      icon.className = 'ci ' + (option.kind === 'folder' ? 'ci-folder' : M.iconFor(option));
      row.appendChild(icon);

      const name = document.createElement('span');
      name.className = 'name';
      name.textContent = option.name;
      row.appendChild(name);

      const path = document.createElement('span');
      path.className = 'path';
      path.textContent = option.path + (option.meta ? ' · ' + option.meta : '');
      row.appendChild(path);

      if (option.changed) {
        const badge = document.createElement('span');
        badge.className = 'badge-changed';
        badge.textContent = option.changed;
        row.appendChild(badge);
      }

      row.addEventListener('pointerenter', () => {
        selected = index;
        renderMenu();
      });
      row.addEventListener('pointerdown', (event) => {
        event.preventDefault();
        pick(option);
      });
      menuScroll.appendChild(row);
    });

    const active = menuScroll.querySelector('.menu-option.selected');
    if (active) active.scrollIntoView({ block: 'nearest' });
  }

  function openMenu() {
    const sel = window.getSelection();
    savedRange =
      sel.rangeCount && input.contains(sel.anchorNode) ? sel.getRangeAt(0).cloneRange() : null;
    open = true;
    query = '';
    selected = 0;
    menu.classList.remove('hidden');
    renderMenu();
  }

  function closeMenu() {
    open = false;
    menu.classList.add('hidden');
  }

  function pick(option) {
    const item = {
      ...option,
      thumb: option.kind === 'image' ? M.tintedThumb('#a46220') : undefined,
    };
    closeMenu();
    insertToken(item, savedRange);
    M.toast('已在句中插入引用「' + item.name + '」');
  }

  input.addEventListener('keydown', (event) => {
    if (!open && (event.key === '@' || (event.key === '2' && event.shiftKey))) {
      event.preventDefault();
      openMenu();
      return;
    }
    if (!open) return;

    if (event.key === 'Escape') {
      event.preventDefault();
      closeMenu();
    } else if (event.key === 'ArrowDown') {
      event.preventDefault();
      selected = (selected + 1) % filtered.length;
      renderMenu();
    } else if (event.key === 'ArrowUp') {
      event.preventDefault();
      selected = (selected - 1 + filtered.length) % filtered.length;
      renderMenu();
    } else if (event.key === 'Enter') {
      event.preventDefault();
      if (filtered[selected]) pick(filtered[selected]);
    } else if (event.key === 'Backspace') {
      event.preventDefault();
      if (!query) closeMenu();
      query = query.slice(0, -1);
      renderMenu();
    } else if (event.key.length === 1 && !event.metaKey && !event.ctrlKey) {
      event.preventDefault();
      query += event.key;
      selected = 0;
      renderMenu();
    }
  });

  document.getElementById('mentionButton').addEventListener('click', () => {
    input.focus();
    openMenu();
  });

  document.addEventListener('pointerdown', (event) => {
    if (!open) return;
    if (menu.contains(event.target) || event.target.closest('#mentionButton')) return;
    closeMenu();
  });

  /* --------------------------------------------------------------- drop */

  M.bindDropZone(composer, {
    onItems: (items) => {
      items.forEach((item) => insertToken(item));
      M.toast(items.length + ' 项引用已插入光标处');
    },
  });

  document.getElementById('sendButton').addEventListener('click', () => {
    M.toast('Mock:发送后,句中引用连同快照进入时间线(见上方已发送示例)');
  });

  /* -------------------------------------------------------------- scenes */

  document.querySelectorAll('.mock-controls [data-act]').forEach((button) => {
    button.addEventListener('click', () => {
      const act = button.dataset.act;
      if (act === 'menu') {
        input.focus();
        openMenu();
      }
      if (act === 'demo') {
        input.focus();
        input.innerHTML = '';
        input.appendChild(document.createTextNode('对照 '));
        insertToken({ ...M.DEMO_ITEMS.screenshot, thumb: M.DEMO_ITEMS.screenshot.thumb });
        input.appendChild(document.createTextNode('把 '));
        insertToken({ kind: 'file', name: 'index.html', path: 'public/index.html' });
        input.appendChild(document.createTextNode('里的按钮间距调成 12px'));
      }
      if (act === 'reset') {
        input.innerHTML = '';
        closeMenu();
      }
    });
  });
})();
