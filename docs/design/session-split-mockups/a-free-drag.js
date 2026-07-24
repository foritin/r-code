const zone = document.getElementById('split-zone');
const main = document.getElementById('pane-main');
const divider = document.getElementById('divider');
const chip = document.getElementById('chip');
const chipText = document.getElementById('chip-text');
const chipPx = document.getElementById('chip-px');
const readout = document.getElementById('ratio');

const MIN_MAIN = 380;
const MIN_TOOLS = 360;
const DEFAULT_PCT = 56;
const STORE_KEY = 'charter-split-a';

let pct = Number(localStorage.getItem(STORE_KEY)) || DEFAULT_PCT;
let limitTimer;

function clampPct(raw) {
  const width = zone.clientWidth;
  const min = (MIN_MAIN / width) * 100;
  const max = 100 - (MIN_TOOLS / width) * 100;
  const clamped = Math.min(Math.max(raw, min), max);
  if (clamped !== raw) {
    divider.classList.add('at-limit');
    clearTimeout(limitTimer);
    limitTimer = setTimeout(() => divider.classList.remove('at-limit'), 360);
  }
  return clamped;
}

function apply(raw, { animate = false, persist = false } = {}) {
  pct = clampPct(raw);
  zone.classList.toggle('animate', animate);
  main.style.width = `${pct}%`;
  const tools = 100 - pct;
  readout.textContent = `对话 ${Math.round(pct)}% · 工具 ${Math.round(tools)}%`;
  chipText.textContent = `对话 ${Math.round(pct)}% · 工具 ${Math.round(tools)}%`;
  chipPx.textContent = `${Math.round((zone.clientWidth * pct) / 100)}px`;
  chip.style.left = `${(zone.clientWidth * pct) / 100}px`;
  if (persist) localStorage.setItem(STORE_KEY, String(pct));
}

divider.addEventListener('pointerdown', (event) => {
  event.preventDefault();
  divider.setPointerCapture(event.pointerId);
  divider.classList.add('dragging');
  chip.classList.add('visible');
});

divider.addEventListener('pointermove', (event) => {
  if (!divider.classList.contains('dragging')) return;
  const left = zone.getBoundingClientRect().left;
  apply(((event.clientX - left) / zone.clientWidth) * 100);
});

divider.addEventListener('pointerup', (event) => {
  divider.releasePointerCapture(event.pointerId);
  divider.classList.remove('dragging');
  chip.classList.remove('visible');
  apply(pct, { persist: true });
});

divider.addEventListener('dblclick', () => {
  apply(DEFAULT_PCT, { animate: true, persist: true });
  chip.classList.add('visible');
  setTimeout(() => chip.classList.remove('visible'), 900);
});

divider.addEventListener('keydown', (event) => {
  if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
  event.preventDefault();
  apply(pct + (event.key === 'ArrowLeft' ? -2 : 2), { animate: true, persist: true });
  chip.classList.add('visible');
  setTimeout(() => chip.classList.remove('visible'), 900);
});

window.addEventListener('resize', () => apply(pct));

apply(pct);
