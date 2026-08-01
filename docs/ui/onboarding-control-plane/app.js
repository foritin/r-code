const tour = document.querySelector(".tour");
const tourHeader = document.querySelector(".tour-header");
const viewport = document.getElementById("slider-viewport");
const track = document.getElementById("slider-track");
const slides = [...document.querySelectorAll(".slide")];
const steps = [...document.querySelectorAll(".port-step")];
const positionLabel = document.getElementById("position-label");
const positionCurrent = document.getElementById("position-current");
const portProgress = document.getElementById("port-progress");
const edgePrev = document.getElementById("edge-prev");
const edgeNext = document.getElementById("edge-next");
const footerNext = document.getElementById("footer-next");
const exitLayer = document.getElementById("exit-layer");

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
  portProgress.style.width = `${(current / (slides.length - 1)) * 100}%`;
  edgePrev.disabled = current === 0;
  edgeNext.disabled = current === slides.length - 1;
  footerNext.innerHTML = current === slides.length - 1
    ? "<span>完成导览</span><i>→</i>"
    : "<span>下一项</span><i>→</i>";
  positionTrack(0, true);
  if (focus) viewport.focus({ preventScroll: true });
}

function setSlide(index, options = {}) {
  current = Math.max(0, Math.min(slides.length - 1, index));
  render(options);
}

function showExit(complete = false) {
  exitLayer.hidden = false;
  document.getElementById("exit-title").textContent = complete ? "这套会话上下文已确认。" : "稍后再完成设置？";
  document.querySelector(".exit-index").textContent = complete ? "BOUND" : "PAUSE";
  document.querySelector(".exit-card p").textContent = complete
    ? "正式实现中，这里会调用后端创建任务，并把 Agent、Provider、模型、工作区与项目权限写入会话记录。"
    : "当前配置不会伪装成已保存。进入工作台后，可从“设置 → 模型服务 / Codex / Agent”继续。";
  document.getElementById("resume-button").focus();
}

function hideExit() {
  exitLayer.hidden = true;
  viewport.focus({ preventScroll: true });
}

function next() {
  if (current === slides.length - 1) showExit(true);
  else setSlide(current + 1, { focus: true });
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
  if ((current === 0 && dragX > 0) || (current === slides.length - 1 && dragX < 0)) dragX *= .26;
  positionTrack(dragX, false);
});

function endSlideDrag(event) {
  if (!dragging || event.pointerId !== pointerId) return;
  const elapsed = Math.max(1, performance.now() - startTime);
  const velocity = dragX / elapsed;
  const threshold = Math.min(120, viewport.clientWidth * .12);
  dragging = false;
  viewport.classList.remove("dragging");
  try { viewport.releasePointerCapture(pointerId); } catch (_) {}
  pointerId = null;
  if ((dragX < -threshold || velocity < -.55) && current < slides.length - 1) current += 1;
  else if ((dragX > threshold || velocity > .55) && current > 0) current -= 1;
  dragX = 0;
  render();
}

viewport.addEventListener("pointerup", endSlideDrag);
viewport.addEventListener("pointercancel", endSlideDrag);

steps.forEach((step) => step.addEventListener("click", () => setSlide(Number(step.dataset.go), { focus: true })));
document.querySelectorAll(".next-action").forEach((button) => button.addEventListener("click", next));
edgeNext.addEventListener("click", next);
edgePrev.addEventListener("click", () => setSlide(current - 1, { focus: true }));
footerNext.addEventListener("click", next);

document.querySelectorAll(".engine-option").forEach((option) => {
  option.addEventListener("click", () => {
    const feedback = document.getElementById("engine-feedback");
    if (option.dataset.engine === "codex") {
      feedback.innerHTML = "Codex CLI 当前不可选：先完成 <code>integration_ready</code> 并附加工作区。";
      option.classList.add("gate-pulse");
      window.setTimeout(() => option.classList.remove("gate-pulse"), 420);
      return;
    }
    feedback.innerHTML = "当前会话将写入 <code>agent_engine=r_code</code>。";
  });
});

document.querySelectorAll(".provider-option").forEach((option) => {
  option.addEventListener("click", () => {
    document.querySelectorAll(".provider-option").forEach((item) => {
      const selected = item === option;
      item.classList.toggle("selected", selected);
      item.setAttribute("aria-checked", String(selected));
    });
    document.getElementById("provider-code").textContent = option.dataset.code;
    document.getElementById("provider-name").textContent = option.dataset.provider;
    document.getElementById("provider-auth").textContent = option.dataset.auth;
    document.getElementById("provider-url").textContent = option.dataset.url;
    document.getElementById("provider-protocol").textContent = option.dataset.protocol;
    document.getElementById("provider-protocol-code").textContent = option.dataset.protocolCode;
    document.getElementById("provider-model").textContent = option.dataset.model;
    document.getElementById("provider-context").textContent = option.dataset.context;
    document.getElementById("provider-output").textContent = option.dataset.output;
    document.getElementById("provider-note").textContent = option.dataset.note;
  });
});

document.getElementById("save-secret").addEventListener("click", () => {
  const field = document.getElementById("secret-field");
  field.classList.add("saved");
  document.getElementById("secret-feedback").textContent = "已安全保存 · source=keychain · WebView 未收到密钥正文";
});

document.querySelectorAll(".policy-option").forEach((option) => {
  option.addEventListener("click", () => {
    document.querySelectorAll(".policy-option").forEach((item) => {
      const selected = item === option;
      item.classList.toggle("selected", selected);
      item.setAttribute("aria-checked", String(selected));
    });
    document.getElementById("launch-access").textContent = `${option.dataset.mode} · ${option.dataset.summary}`;
  });
});

document.getElementById("workspace-button").addEventListener("click", () => {
  document.getElementById("workspace-button").textContent = "已选择";
});

document.getElementById("launch-action").addEventListener("click", () => showExit(true));
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

let movingWindow = false;
let movePointer = null;
let moveStartX = 0;
let moveStartY = 0;
let moveLeft = 0;
let moveTop = 0;

tourHeader.addEventListener("pointerdown", (event) => {
  if (event.button !== 0 || isInteractive(event.target)) return;
  const rect = tour.getBoundingClientRect();
  movingWindow = true;
  movePointer = event.pointerId;
  moveStartX = event.clientX;
  moveStartY = event.clientY;
  moveLeft = rect.left;
  moveTop = rect.top;
  tour.style.left = `${rect.left}px`;
  tour.style.top = `${rect.top}px`;
  tour.style.transform = "none";
  tourHeader.setPointerCapture(movePointer);
  tourHeader.classList.add("moving");
});

tourHeader.addEventListener("pointermove", (event) => {
  if (!movingWindow || event.pointerId !== movePointer) return;
  const maxLeft = Math.max(0, window.innerWidth - tour.offsetWidth);
  const maxTop = Math.max(0, window.innerHeight - tour.offsetHeight);
  const left = Math.max(0, Math.min(maxLeft, moveLeft + event.clientX - moveStartX));
  const top = Math.max(0, Math.min(maxTop, moveTop + event.clientY - moveStartY));
  tour.style.left = `${left}px`;
  tour.style.top = `${top}px`;
});

function endWindowMove(event) {
  if (!movingWindow || event.pointerId !== movePointer) return;
  movingWindow = false;
  tourHeader.classList.remove("moving");
  try { tourHeader.releasePointerCapture(movePointer); } catch (_) {}
  movePointer = null;
}

tourHeader.addEventListener("pointerup", endWindowMove);
tourHeader.addEventListener("pointercancel", endWindowMove);

window.addEventListener("resize", () => {
  positionTrack(0, false);
  if (tour.style.transform === "none") {
    const rect = tour.getBoundingClientRect();
    tour.style.left = `${Math.max(0, Math.min(window.innerWidth - rect.width, rect.left))}px`;
    tour.style.top = `${Math.max(0, Math.min(window.innerHeight - rect.height, rect.top))}px`;
  }
});

window.controlPlaneTour = {
  setSlide,
  get current() { return current; },
};

render();
