/* Charter · context-attach mockups shared helpers. NOT product code. */
(function () {
  'use strict';

  const REF_MIME = 'application/x-charter-ref';

  function toast(message) {
    const el = document.getElementById('toast');
    if (!el) return;
    el.textContent = message;
    el.classList.add('visible');
    clearTimeout(toast._t);
    toast._t = setTimeout(() => el.classList.remove('visible'), 2600);
  }

  function prettySize(bytes) {
    if (!Number.isFinite(bytes)) return '';
    if (bytes < 1024) return bytes + ' B';
    if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB';
    return (bytes / 1024 / 1024).toFixed(1) + ' MB';
  }

  function iconFor(item) {
    if (item.kind === 'folder') return 'ci-folder';
    if (item.kind === 'image') return 'ci-file-media';
    const name = item.name || '';
    if (/\.md$/i.test(name)) return 'ci-markdown';
    if (/\.json$/i.test(name)) return 'ci-json';
    if (/\.(ts|tsx|js|jsx|html|css)$/i.test(name)) return 'ci-file-code';
    return 'ci-file';
  }

  /* One attachable item: { kind: 'file'|'folder'|'image', name, path?, meta?, thumb? } */
  function chipNode(item, onRemove) {
    const chip = document.createElement('span');
    chip.className = 'ref-chip is-' + item.kind;
    chip.dataset.key = item.path || item.name;

    if (item.kind === 'image' && item.thumb) {
      const img = document.createElement('img');
      img.className = 'thumb';
      img.alt = '';
      img.src = item.thumb;
      chip.appendChild(img);
    } else {
      const icon = document.createElement('span');
      icon.className = 'ci ' + iconFor(item);
      chip.appendChild(icon);
    }

    const name = document.createElement('span');
    name.className = 'name';
    name.textContent = item.name;
    name.title = item.path || item.name;
    chip.appendChild(name);

    if (item.meta) {
      const meta = document.createElement('span');
      meta.className = 'meta';
      meta.textContent = item.meta;
      chip.appendChild(meta);
    }

    const remove = document.createElement('button');
    remove.className = 'remove';
    remove.type = 'button';
    remove.title = '移除';
    remove.innerHTML = '<span class="ci ci-close"></span>';
    remove.addEventListener('click', () => {
      chip.remove();
      if (onRemove) onRemove(item, chip);
    });
    chip.appendChild(remove);
    return chip;
  }

  /* ------------------------------------------------ OS drag payload readers */

  function countEntries(dirEntry, limit) {
    return new Promise((resolve) => {
      let total = 0;
      let pending = 0;
      let capped = false;

      function walk(entry) {
        pending += 1;
        const reader = entry.createReader();
        const batch = () => {
          reader.readEntries((entries) => {
            if (capped) return finish();
            for (const child of entries) {
              total += 1;
              if (total >= limit) {
                capped = true;
                break;
              }
              if (child.isDirectory) walk(child);
            }
            if (entries.length > 0 && !capped) return batch();
            finish();
          }, finish);
        };
        const finish = () => {
          pending -= 1;
          if (pending === 0) resolve({ total, capped });
        };
        batch();
      }

      walk(dirEntry);
      setTimeout(() => resolve({ total, capped: true }), 1200); /* mock safety net */
    });
  }

  async function readDataTransfer(dt) {
    const items = [];
    const rawItems = dt.items ? Array.from(dt.items) : [];
    const files = dt.files ? Array.from(dt.files) : [];

    if (rawItems.length && rawItems[0].webkitGetAsEntry) {
      for (const raw of rawItems) {
        if (raw.kind !== 'file') continue;
        const entry = raw.webkitGetAsEntry();
        const file = raw.getAsFile ? raw.getAsFile() : null;
        if (entry && entry.isDirectory) {
          const { total, capped } = await countEntries(entry, 400);
          items.push({
            kind: 'folder',
            name: entry.name,
            path: entry.fullPath || entry.name,
            meta: (capped ? '≥' : '') + total + ' 项',
          });
        } else if (file) {
          items.push(fileToItem(file));
        }
      }
      return items;
    }

    return files.map(fileToItem);
  }

  function fileToItem(file) {
    const isImage = /^image\//.test(file.type);
    return {
      kind: isImage ? 'image' : 'file',
      name: file.name,
      path: file.name,
      meta: prettySize(file.size),
      thumb: isImage ? URL.createObjectURL(file) : undefined,
    };
  }

  /* -------------------------------------------------- in-app drag payloads */

  function setRefPayload(event, item) {
    event.dataTransfer.setData(REF_MIME, JSON.stringify(item));
    event.dataTransfer.setData('text/plain', item.path || item.name);
    event.dataTransfer.effectAllowed = 'copy';
  }

  function getRefPayload(dt) {
    const raw = dt.getData(REF_MIME);
    if (!raw) return null;
    try {
      return JSON.parse(raw);
    } catch {
      return null;
    }
  }

  function dragHasPayload(dt) {
    const types = Array.from(dt.types || []);
    return types.includes(REF_MIME) || types.includes('Files');
  }

  /* Depth-tracked drop zone: onItems(items[]) fires for both OS files and
     in-app tree refs. hoverClass toggles on the zone while a drag is above. */
  function bindDropZone(zone, options) {
    const hoverClass = options.hoverClass || 'drag-over';
    const target = options.highlightTarget || zone;
    let depth = 0;

    zone.addEventListener('dragenter', (event) => {
      if (!dragHasPayload(event.dataTransfer)) return;
      event.preventDefault();
      depth += 1;
      target.classList.add(hoverClass);
    });

    zone.addEventListener('dragover', (event) => {
      if (!dragHasPayload(event.dataTransfer)) return;
      event.preventDefault();
      event.dataTransfer.dropEffect = 'copy';
    });

    zone.addEventListener('dragleave', () => {
      depth = Math.max(0, depth - 1);
      if (depth === 0) target.classList.remove(hoverClass);
    });

    zone.addEventListener('drop', async (event) => {
      if (!dragHasPayload(event.dataTransfer)) return;
      event.preventDefault();
      depth = 0;
      target.classList.remove(hoverClass);
      const ref = getRefPayload(event.dataTransfer);
      const items = ref ? [ref] : await readDataTransfer(event.dataTransfer);
      if (items.length) options.onItems(items, event);
    });
  }

  /* 1×1 px PNGs tinted like the archive palette, for simulated image chips. */
  function tintedThumb(hex) {
    const canvas = document.createElement('canvas');
    canvas.width = canvas.height = 32;
    const ctx = canvas.getContext('2d');
    ctx.fillStyle = hex;
    ctx.fillRect(0, 0, 32, 32);
    ctx.fillStyle = 'rgba(255,255,255,0.55)';
    ctx.fillRect(5, 18, 22, 9);
    ctx.beginPath();
    ctx.arc(11, 11, 4.5, 0, Math.PI * 2);
    ctx.fill();
    return canvas.toDataURL('image/png');
  }

  const DEMO_ITEMS = {
    file: { kind: 'file', name: 'checkout.ts', path: 'src/cart/checkout.ts', meta: '4.1 KB' },
    style: { kind: 'file', name: 'index.html', path: 'public/index.html', meta: '+5 −2' },
    folder: { kind: 'folder', name: 'styles', path: 'public/styles', meta: '6 项' },
    image: {
      kind: 'image',
      name: 'coupon-expired@2x.png',
      path: 'assets/coupon-expired@2x.png',
      meta: '96 KB',
      get thumb() {
        return tintedThumb('#b94e32');
      },
    },
    screenshot: {
      kind: 'image',
      name: '截屏 2026-07-18 11.02.14.png',
      path: '~/Desktop/截屏 2026-07-18 11.02.14.png',
      meta: '412 KB',
      get thumb() {
        return tintedThumb('#3f6674');
      },
    },
  };

  window.CharterMock = {
    REF_MIME,
    toast,
    prettySize,
    iconFor,
    chipNode,
    readDataTransfer,
    setRefPayload,
    getRefPayload,
    bindDropZone,
    tintedThumb,
    DEMO_ITEMS,
  };
})();
