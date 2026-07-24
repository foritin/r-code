(() => {
  const root = document.querySelector('[data-replay]');
  if (!root) return;

  const duration = Number(root.dataset.duration || 38);
  let current = Number(root.dataset.start || 0);
  let playing = false;
  let speed = 1;
  let previousFrame = performance.now();
  let toastTimer = 0;

  const captureSources = [
    {
      key: 'pi',
      label: 'Pi Home',
      grade: '完整记录',
      detail: '消息 · 工具 · Patch · 权限 · 验证',
    },
    {
      key: 'claude',
      label: 'Claude Terminal',
      grade: '观察模式',
      detail: '当前仅会话 · PTY · 文件净变化',
    },
    {
      key: 'codex',
      label: 'Codex Terminal',
      grade: '观察模式',
      detail: '当前仅会话 · PTY · 文件净变化',
    },
  ];

  const appbar = root.querySelector('.appbar');
  const directionNav = root.querySelector('.direction-nav');
  let captureKey = 'pi';
  if (appbar && directionNav) {
    const picker = document.createElement('div');
    picker.className = 'capture-picker';
    picker.innerHTML = `
      <button class="capture-button" type="button" aria-haspopup="menu" aria-expanded="false">
        <i class="capture-grade-dot"></i>
        <span class="capture-source"><b>Pi Home</b><small>完整记录</small></span>
        <svg class="capture-chevron" viewBox="0 0 16 16" fill="none" stroke="currentColor"><path d="m4 6 4 4 4-4"/></svg>
      </button>
      <div class="capture-menu" role="menu">
        ${captureSources
          .map(
            (source) =>
              `<button class="capture-option ${source.key === 'pi' ? '' : 'observe'}" data-capture-key="${source.key}" role="menuitem"><i></i><span><b>${source.label}</b><small>${source.detail}</small></span><em>${source.grade}</em></button>`,
          )
          .join('')}
        <div class="capture-menu-foot">Claude stream-json / hooks 与 Codex JSON / app-server 可以升级为结构化记录；普通 TUI 默认保持观察模式。</div>
      </div>`;
    appbar.insertBefore(picker, directionNav);
    const button = picker.querySelector('.capture-button');

    const setCapture = (key) => {
      const source = captureSources.find((item) => item.key === key) || captureSources[0];
      captureKey = source.key;
      root.dataset.capture = source.key;
      root.classList.toggle('capture-observed', source.key !== 'pi');
      picker.querySelector('.capture-source b').textContent = source.label;
      picker.querySelector('.capture-source small').textContent = source.grade;
      picker
        .querySelectorAll('.capture-option')
        .forEach((option) =>
          option.classList.toggle('on', option.dataset.captureKey === source.key),
        );
      root.querySelectorAll('[data-capture-note]').forEach((note) => {
        note.textContent = note.dataset[source.key] || '';
      });
      picker.classList.remove('open');
      button.setAttribute('aria-expanded', 'false');
      root.dispatchEvent(new CustomEvent('capturechange', { detail: source }));
    };

    button.addEventListener('click', () => {
      const open = picker.classList.toggle('open');
      button.setAttribute('aria-expanded', String(open));
    });
    picker
      .querySelectorAll('.capture-option')
      .forEach((option) =>
        option.addEventListener('click', () => setCapture(option.dataset.captureKey)),
      );
    document.addEventListener('click', (event) => {
      if (!picker.contains(event.target)) {
        picker.classList.remove('open');
        button.setAttribute('aria-expanded', 'false');
      }
    });
    setCapture('pi');
  }

  const scrubber = root.querySelector('.js-scrubber');
  const currentLabel = root.querySelector('.js-current-time');
  const durationLabel = root.querySelector('.js-duration');
  const captionTitle = root.querySelector('.js-caption-title');
  const captionDetail = root.querySelector('.js-caption-detail');
  const captions = [...root.querySelectorAll('[data-caption-at]')]
    .map((el) => ({
      at: Number(el.dataset.captionAt),
      title: el.dataset.title || '',
      detail: el.dataset.detail || '',
    }))
    .sort((a, b) => a.at - b.at);

  const format = (value) => {
    const seconds = Math.max(0, Math.round(value));
    return `${String(Math.floor(seconds / 60)).padStart(2, '0')}:${String(seconds % 60).padStart(2, '0')}`;
  };

  const toast = (message) => {
    const node = root.querySelector('.toast');
    if (!node) return;
    node.textContent = message;
    node.classList.add('show');
    window.clearTimeout(toastTimer);
    toastTimer = window.setTimeout(() => node.classList.remove('show'), 1700);
  };

  const seek = (next, source = 'seek') => {
    current = Math.max(0, Math.min(duration, next));
    if (current >= duration && source === 'tick') playing = false;
    update();
  };

  const sequenceParents = [...root.querySelectorAll('[data-sequence]')];
  const updateSequence = (parent) => {
    const items = [...parent.querySelectorAll(':scope > [data-at], :scope > * [data-at]')];
    let active = null;
    for (const item of items) {
      const at = Number(item.dataset.at || 0);
      item.classList.toggle('reached', current >= at);
      item.classList.remove('active');
      if (current >= at && (!active || at >= Number(active.dataset.at || 0))) active = item;
    }
    active?.classList.add('active');
  };

  const update = () => {
    const percent = (current / duration) * 100;
    root.style.setProperty('--progress', `${percent}%`);
    root.classList.toggle('is-playing', playing);
    if (scrubber) scrubber.value = String(current);
    if (currentLabel) currentLabel.textContent = format(current);
    if (durationLabel) durationLabel.textContent = format(duration);

    root
      .querySelectorAll('[data-at]')
      .forEach((item) => item.classList.toggle('reached', current >= Number(item.dataset.at || 0)));
    root.querySelectorAll('[data-window]').forEach((item) => {
      const [start, end] = item.dataset.window.split('-').map(Number);
      item.classList.toggle('in-window', current >= start && current < end);
    });
    sequenceParents.forEach(updateSequence);

    let caption = captions[0];
    for (const next of captions) if (current >= next.at) caption = next;
    if (captionTitle && caption) captionTitle.textContent = caption.title;
    if (captionDetail && caption) captionDetail.textContent = caption.detail;

    root.dispatchEvent(
      new CustomEvent('replaytick', {
        detail: { current, duration, percent, playing, capture: captureKey },
      }),
    );
  };

  const toggle = () => {
    if (current >= duration) current = 0;
    playing = !playing;
    previousFrame = performance.now();
    update();
  };

  root.querySelector('.js-play')?.addEventListener('click', toggle);
  root.querySelector('.js-restart')?.addEventListener('click', () => {
    playing = true;
    seek(0);
  });
  root.querySelector('.js-back')?.addEventListener('click', () => seek(current - 5));
  root.querySelector('.js-forward')?.addEventListener('click', () => seek(current + 5));
  root.querySelector('.js-speed')?.addEventListener('click', (event) => {
    speed = speed === 1 ? 2 : speed === 2 ? 0.5 : 1;
    event.currentTarget.textContent = `${speed}×`;
    toast(`播放速度 ${speed}×`);
  });
  root.querySelector('.js-fullscreen')?.addEventListener('click', () => {
    if (!document.fullscreenElement) root.requestFullscreen?.();
    else document.exitFullscreen?.();
  });
  scrubber?.addEventListener('input', (event) => {
    playing = false;
    seek(Number(event.target.value));
  });
  root.querySelectorAll('[data-seek]').forEach((node) =>
    node.addEventListener('click', () => {
      playing = false;
      seek(Number(node.dataset.seek));
    }),
  );
  root
    .querySelectorAll('[data-toast]')
    .forEach((node) => node.addEventListener('click', () => toast(node.dataset.toast)));

  document.addEventListener('keydown', (event) => {
    if (event.target.matches('input, textarea')) return;
    if (event.code === 'Space') {
      event.preventDefault();
      toggle();
    }
    if (event.key === 'ArrowLeft') seek(current - 2);
    if (event.key === 'ArrowRight') seek(current + 2);
  });

  const frame = (now) => {
    const delta = Math.min((now - previousFrame) / 1000, 0.1);
    previousFrame = now;
    if (playing) seek(current + delta * speed, 'tick');
    requestAnimationFrame(frame);
  };

  if (scrubber) {
    scrubber.min = '0';
    scrubber.max = String(duration);
    scrubber.step = '.05';
  }
  update();
  requestAnimationFrame(frame);
  if (root.dataset.autoplay === 'true')
    window.setTimeout(() => {
      playing = true;
      previousFrame = performance.now();
      update();
    }, 900);
})();
