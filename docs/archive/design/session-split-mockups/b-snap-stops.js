const zone = document.getElementById('split-zone');
const main = document.getElementById('pane-main');
const divider = document.getElementById('divider');
const chip = document.getElementById('chip');
const chipText = document.getElementById('chip-text');
const chipSub = document.getElementById('chip-sub');
const readout = document.getElementById('ratio');
const notches = [...document.querySelectorAll('.stops button')];

const STOPS = [
  { pct: 62, label: '对话优先' },
  { pct: 50, label: '均衡' },
  { pct: 38, label: '工具优先' },
];
const MAGNET_PCT = 3.5;

let current = 0;
let livePct = STOPS[0].pct;
let chipTimer;

function stopAt(pct) {
  let best = 0;
  for (let i = 1; i < STOPS.length; i += 1) {
    if (Math.abs(STOPS[i].pct - pct) < Math.abs(STOPS[best].pct - pct)) best = i;
  }
  return best;
}

function paint(pct, { animate = false } = {}) {
  livePct = pct;
  zone.classList.toggle('animate', animate);
  main.style.width = `${pct}%`;
  chip.style.left = `${(zone.clientWidth * pct) / 100}px`;
}

function flashChip(text, sub) {
  chipText.textContent = text;
  chipSub.textContent = sub;
  chip.classList.add('visible');
  clearTimeout(chipTimer);
  chipTimer = setTimeout(() => chip.classList.remove('visible'), 1100);
}

function settle(index, { announce = true } = {}) {
  current = index;
  const stop = STOPS[index];
  paint(stop.pct, { animate: true });
  notches.forEach((notch, i) => {
    notch.classList.toggle('on', i === index);
    notch.classList.remove('near');
  });
  readout.textContent = `${stop.label} · ${stop.pct}% / ${100 - stop.pct}%`;
  if (announce) flashChip(stop.label, `${stop.pct} / ${100 - stop.pct}`);
}

divider.addEventListener('pointerdown', (event) => {
  if (event.target.closest('.stops button')) return;
  event.preventDefault();
  divider.setPointerCapture(event.pointerId);
  divider.classList.add('dragging');
  chip.classList.add('visible');
  clearTimeout(chipTimer);
});

divider.addEventListener('pointermove', (event) => {
  if (!divider.classList.contains('dragging')) return;
  const left = zone.getBoundingClientRect().left;
  let pct = ((event.clientX - left) / zone.clientWidth) * 100;
  const near = stopAt(pct);
  const pull = Math.abs(STOPS[near].pct - pct) <= MAGNET_PCT;
  if (pull) pct = STOPS[near].pct + (pct - STOPS[near].pct) * 0.3;
  notches.forEach((notch, i) => notch.classList.toggle('near', pull && i === near));
  paint(Math.min(Math.max(pct, 30), 70));
  chipText.textContent = pull ? STOPS[near].label : '松手吸附到最近档位';
  chipSub.textContent = `${Math.round(livePct)} / ${Math.round(100 - livePct)}`;
});

divider.addEventListener('pointerup', (event) => {
  divider.releasePointerCapture(event.pointerId);
  divider.classList.remove('dragging');
  settle(stopAt(livePct));
});

divider.addEventListener('dblclick', () => settle((current + 1) % STOPS.length));

divider.addEventListener('keydown', (event) => {
  if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
  event.preventDefault();
  const next = event.key === 'ArrowLeft' ? current + 1 : current - 1;
  settle(Math.min(Math.max(next, 0), STOPS.length - 1));
});

notches.forEach((notch, index) => {
  notch.addEventListener('click', () => settle(index));
});

window.addEventListener('resize', () => paint(STOPS[current].pct));

settle(0, { announce: false });
