const viewport = document.getElementById("slider-viewport");
const track = document.getElementById("slider-track");
const slides = [...document.querySelectorAll(".slide")];
const steps = [...document.querySelectorAll(".ribbon-step")];
const positionLabel = document.getElementById("position-label");
const positionCurrent = document.getElementById("position-current");
const nextPeek = document.getElementById("next-peek");
const meter = document.getElementById("slide-meter-fill");
const edgePrev = document.getElementById("edge-prev");
const edgeNext = document.getElementById("edge-next");
const footerNext = document.getElementById("footer-next");
const exitLayer = document.getElementById("exit-layer");
const keyField = document.getElementById("key-field");
const keyInput = document.getElementById("api-key");

const params = new URLSearchParams(location.search);
let current = Math.max(0, Math.min(slides.length - 1, Number(params.get("slide")) - 1 || 0));
let dragging = false;
let pointerId = null;
let startX = 0;
let dragX = 0;
let startTime = 0;

function slideOffset(index = current, delta = 0) {
  return -(index * viewport.clientWidth) + delta;
}

function positionTrack(delta = 0, animate = true) {
  track.classList.toggle("dragging", !animate);
  track.style.transform = `translate3d(${slideOffset(current, delta)}px, 0, 0)`;
}

function render({ focus = false } = {}) {
  slides.forEach((slide, index) => slide.setAttribute("aria-hidden", String(index !== current)));
  steps.forEach((step, index) => {
    step.classList.toggle("active", index === current);
    step.classList.toggle("done", index < current);
    step.setAttribute("aria-current", index === current ? "step" : "false");
  });
  positionLabel.textContent = slides[current].dataset.label;
  positionCurrent.textContent = String(current + 1).padStart(2, "0");
  nextPeek.textContent = current < slides.length - 1 ? `NEXT · ${String(current + 2).padStart(2, "0")}` : "DONE";
  meter.style.width = `${((current + 1) / slides.length) * 100}%`;
  edgePrev.disabled = current === 0;
  edgeNext.disabled = current === slides.length - 1;
  footerNext.innerHTML = current === slides.length - 1 ? "完成 <span>→</span>" : "下一步 <span>→</span>";
  const credentialReady = Boolean(keyInput.value.trim());
  const readyRow = document.getElementById("credential-ready-row");
  readyRow.classList.toggle("incomplete", !credentialReady);
  readyRow.innerHTML = credentialReady
    ? "<b>02</b> 凭据已安全保存 <i>✓</i>"
    : "<b>02</b> 凭据尚未保存 <i>!</i>";
  positionTrack(0, true);
  if (focus) viewport.focus({ preventScroll: true });
}

function setSlide(index, options = {}) {
  current = Math.max(0, Math.min(slides.length - 1, index));
  render(options);
}

function goNext() {
  if (current === 2 && !keyInput.value.trim()) {
    keyField.classList.add("invalid");
    keyInput.focus();
    return;
  }
  if (current === slides.length - 1) {
    showExit(true);
    return;
  }
  setSlide(current + 1, { focus: true });
}

function showExit(complete = false) {
  exitLayer.hidden = false;
  const title = document.getElementById("exit-title");
  title.textContent = complete ? "准备好进入 R-Code 了吗？" : "稍后再完成设置？";
  document.querySelector(".exit-ribbon").textContent = complete ? "SETUP / COMPLETE" : "PAUSE / SETUP";
  document.getElementById("resume-button").focus();
}

function hideExit() {
  exitLayer.hidden = true;
  viewport.focus({ preventScroll: true });
}

function isInteractive(target) {
  return Boolean(target.closest("button, input, textarea, select, a, label"));
}

viewport.addEventListener("pointerdown", (event) => {
  if (event.button !== 0 || isInteractive(event.target)) return;
  dragging = true;
  pointerId = event.pointerId;
  startX = event.clientX;
  startTime = performance.now();
  dragX = 0;
  viewport.classList.add("dragging");
  viewport.setPointerCapture(pointerId);
  positionTrack(0, false);
});

viewport.addEventListener("pointermove", (event) => {
  if (!dragging || event.pointerId !== pointerId) return;
  dragX = event.clientX - startX;
  if ((current === 0 && dragX > 0) || (current === slides.length - 1 && dragX < 0)) dragX *= .28;
  positionTrack(dragX, false);
});

function endDrag(event) {
  if (!dragging || event.pointerId !== pointerId) return;
  const elapsed = Math.max(1, performance.now() - startTime);
  const velocity = dragX / elapsed;
  const threshold = Math.min(110, viewport.clientWidth * .13);
  dragging = false;
  viewport.classList.remove("dragging");
  try { viewport.releasePointerCapture(pointerId); } catch (_) {}
  pointerId = null;
  if ((dragX < -threshold || velocity < -.55) && current < slides.length - 1) current += 1;
  else if ((dragX > threshold || velocity > .55) && current > 0) current -= 1;
  dragX = 0;
  render();
}

viewport.addEventListener("pointerup", endDrag);
viewport.addEventListener("pointercancel", endDrag);

steps.forEach((step) => step.addEventListener("click", () => setSlide(Number(step.dataset.go), { focus: true })));
document.querySelectorAll(".next-action").forEach((button) => button.addEventListener("click", goNext));
edgeNext.addEventListener("click", goNext);
edgePrev.addEventListener("click", () => setSlide(current - 1, { focus: true }));
footerNext.addEventListener("click", goNext);
document.getElementById("enter-action").addEventListener("click", () => {
  if (!keyInput.value.trim()) {
    setSlide(2);
    keyField.classList.add("invalid");
    keyInput.focus();
    return;
  }
  showExit(true);
});

document.querySelectorAll(".provider-option").forEach((option) => {
  option.addEventListener("click", () => {
    document.querySelectorAll(".provider-option").forEach((item) => {
      const selected = item === option;
      item.classList.toggle("selected", selected);
      item.setAttribute("aria-checked", String(selected));
    });
    const provider = option.dataset.provider;
    const code = option.querySelector(".provider-code").textContent;
    document.getElementById("preset-provider").textContent = provider;
    document.getElementById("preset-logo").textContent = code;
    document.getElementById("preset-model").textContent = option.dataset.model;
    document.getElementById("preset-protocol").textContent = option.dataset.protocol;
    document.getElementById("preset-url").textContent = option.dataset.url;
    document.getElementById("credential-provider").textContent = provider;
    document.getElementById("credential-logo").textContent = code;
    document.getElementById("provider-model").value = option.dataset.model;
    document.getElementById("provider-protocol").textContent = option.dataset.protocol;
    document.getElementById("provider-url").value = option.dataset.url;
  });
});

document.getElementById("toggle-key").addEventListener("click", () => {
  keyInput.type = keyInput.type === "password" ? "text" : "password";
});
keyInput.addEventListener("input", () => {
  keyField.classList.remove("invalid");
  render();
});
document.getElementById("test-connection").addEventListener("click", () => {
  if (!keyInput.value.trim()) {
    keyField.classList.add("invalid");
    keyInput.focus();
    return;
  }
  document.getElementById("connection-state").hidden = false;
});

document.getElementById("skip-button").addEventListener("click", () => showExit(false));
document.getElementById("close-button").addEventListener("click", () => showExit(false));
document.getElementById("resume-button").addEventListener("click", hideExit);
document.getElementById("leave-button").addEventListener("click", () => { exitLayer.hidden = true; });

document.addEventListener("keydown", (event) => {
  if (!exitLayer.hidden) {
    if (event.key === "Escape") hideExit();
    return;
  }
  if (event.key === "Escape") showExit(false);
  if (event.key === "ArrowRight") setSlide(current + 1, { focus: true });
  if (event.key === "ArrowLeft") setSlide(current - 1, { focus: true });
});

window.addEventListener("resize", () => positionTrack(0, false));
window.ribbonTour = { setSlide, get current() { return current; } };
render();
