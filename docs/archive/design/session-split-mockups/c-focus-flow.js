const zone = document.getElementById('split-zone');
const main = document.getElementById('pane-main');
const tools = document.getElementById('pane-tools');
const flip = document.getElementById('flip');
const pin = document.getElementById('pin');
const modeChip = document.getElementById('mode-chip');
const readout = document.getElementById('ratio');

const ATTRACTORS = {
  neutral: { pct: 56, label: '初始' },
  main: { pct: 62, label: '对话优先' },
  tools: { pct: 38, label: '工具优先' },
};
const DWELL_MS = 450;

let mode = 'neutral';
let pct = ATTRACTORS.neutral.pct;
let pinned = false;
const dwellTimers = new Map();

function render() {
  main.style.width = `${pct}%`;
  modeChip.style.left = `${(zone.clientWidth * pct) / 100}px`;
  const split = `${Math.round(pct)} / ${Math.round(100 - pct)}`;
  modeChip.classList.toggle('pinned', pinned);
  modeChip.textContent = pinned ? `已钉住 · ${split}` : `AUTO · ${ATTRACTORS[mode].label} ${split}`;
  readout.textContent = pinned
    ? `已钉住 · 对话 ${Math.round(pct)}% · 工具 ${Math.round(100 - pct)}%`
    : `AUTO · 对话 ${Math.round(pct)}% · 工具 ${Math.round(100 - pct)}%`;
}

function focusPane(next) {
  if (pinned || mode === next) return;
  mode = next;
  pct = ATTRACTORS[next].pct;
  render();
}

function watch(pane, target) {
  pane.addEventListener('pointerdown', () => focusPane(target));
  pane.addEventListener('pointerenter', () => {
    dwellTimers.set(target, setTimeout(() => focusPane(target), DWELL_MS));
  });
  pane.addEventListener('pointerleave', () => {
    clearTimeout(dwellTimers.get(target));
  });
}

watch(main, 'main');
watch(tools, 'tools');

flip.addEventListener('click', () => {
  if (pinned) {
    pct = 100 - pct;
  } else {
    mode = pct >= 50 ? 'tools' : 'main';
    pct = ATTRACTORS[mode].pct;
  }
  render();
});

pin.addEventListener('click', () => {
  pinned = !pinned;
  pin.classList.toggle('on', pinned);
  pin.setAttribute('aria-pressed', String(pinned));
  render();
});

window.addEventListener('resize', render);

render();
