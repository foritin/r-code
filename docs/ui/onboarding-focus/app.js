const tour = document.querySelector(".tour");
const tourHeader = document.querySelector(".tour-header");
const viewport = document.getElementById("slider-viewport");
const track = document.getElementById("slider-track");
const slides = [...document.querySelectorAll(".slide")];
const dots = [...document.querySelectorAll(".dot")];
const backButton = document.getElementById("back-button");
const nextButton = document.getElementById("next-button");
const dialogLayer = document.getElementById("dialog-layer");
const sectionLabel = document.getElementById("section-label");
const currentLabel = document.getElementById("current-label");

const params = new URLSearchParams(location.search);
let current = Math.max(0, Math.min(slides.length - 1, Number(params.get("slide")) - 1 || 0));
let savedKey = false;
let workspaceAttached = true;
let selectedProvider = "DeepSeek";
let selectedModel = "deepseek-v4-flash";
let selectedAccess = "替我审批";
let selectedAccessSummary = "R2/R3 询问";

let dragging = false;
let pointerId = null;
let startX = 0;
let dragX = 0;
let dragStartTime = 0;

function trackOffset(index = current, delta = 0) {
  return -(index * viewport.clientWidth) + delta;
}

function positionTrack(delta = 0, animate = true) {
  track.classList.toggle("dragging", !animate);
  track.style.transform = `translate3d(${trackOffset(current, delta)}px, 0, 0)`;
}

function updateReadyState() {
  const eyebrow = document.getElementById("ready-eyebrow");
  const title = document.getElementById("ready-title");
  const copy = document.getElementById("ready-copy");
  const action = document.getElementById("create-session");
  if (savedKey) {
    eyebrow.textContent = "SESSION READY";
    title.innerHTML = "就这样。<br />可以开始了。";
    copy.textContent = "这些选择属于即将创建的会话，不会被之后的全局默认静默改写。";
    action.innerHTML = "<span>创建第一个会话</span><i>→</i>";
  } else {
    eyebrow.textContent = "SESSION REVIEW";
    title.innerHTML = "确认一下。<br />再开始。";
    copy.textContent = "创建时会检查模型服务是否就绪；这些选择不会被之后的全局默认静默改写。";
    action.innerHTML = "<span>检查并创建</span><i>→</i>";
  }
}

function render({ focus = false } = {}) {
  slides.forEach((slide, index) => slide.setAttribute("aria-hidden", String(index !== current)));
  dots.forEach((dot, index) => {
    dot.classList.toggle("active", index === current);
    dot.setAttribute("aria-current", index === current ? "step" : "false");
  });
  sectionLabel.textContent = slides[current].dataset.label;
  currentLabel.textContent = String(current + 1).padStart(2, "0");
  backButton.disabled = current === 0;
  nextButton.innerHTML = current === slides.length - 1
    ? "<span>完成</span><i>→</i>"
    : "<span>下一步</span><i>→</i>";
  updateReadyState();
  positionTrack(0, true);
  if (focus) viewport.focus({ preventScroll: true });
}

function setSlide(index, options = {}) {
  current = Math.max(0, Math.min(slides.length - 1, index));
  render(options);
}

function validateBeforeCreate() {
  if (!savedKey) {
    setSlide(2);
    const keyField = document.getElementById("key-field");
    keyField.classList.add("error");
    document.getElementById("key-feedback").textContent = "请先填写并保存访问密钥；已有系统凭据时正式产品会直接显示已保存。";
    document.getElementById("api-key").focus();
    return false;
  }
  return true;
}

function goNext() {
  if (current === slides.length - 1) {
    if (validateBeforeCreate()) showDialog(true);
    return;
  }
  setSlide(current + 1, { focus: true });
}

function showDialog(complete = false) {
  dialogLayer.hidden = false;
  document.getElementById("dialog-mark").textContent = complete ? "READY" : "PAUSE";
  document.getElementById("dialog-title").textContent = complete ? "会话配置已确认。" : "稍后再设置？";
  document.getElementById("dialog-copy").textContent = complete
    ? "这是交互原型：正式实现会在这里调用后端创建会话；当前页面不会写入配置、凭据或数据库。"
    : "进入工作台后，可以从设置继续；当前原型不会写入任何配置。";
  document.getElementById("resume-button").focus();
}

function hideDialog() {
  dialogLayer.hidden = true;
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
  dragStartTime = performance.now();
  dragX = 0;
  viewport.classList.add("dragging");
  viewport.setPointerCapture(pointerId);
  positionTrack(0, false);
});

viewport.addEventListener("pointermove", (event) => {
  if (!dragging || event.pointerId !== pointerId) return;
  dragX = event.clientX - startX;
  if ((current === 0 && dragX > 0) || (current === slides.length - 1 && dragX < 0)) dragX *= .25;
  positionTrack(dragX, false);
});

function endSlideDrag(event) {
  if (!dragging || event.pointerId !== pointerId) return;
  const elapsed = Math.max(1, performance.now() - dragStartTime);
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

dots.forEach((dot) => dot.addEventListener("click", () => setSlide(Number(dot.dataset.go), { focus: true })));
document.querySelectorAll(".next-button").forEach((button) => button.addEventListener("click", goNext));
backButton.addEventListener("click", () => setSlide(current - 1, { focus: true }));
nextButton.addEventListener("click", goNext);

document.querySelectorAll(".engine-choice").forEach((option) => {
  option.addEventListener("click", () => {
    const feedback = document.getElementById("engine-feedback");
    if (option.dataset.engine === "codex") {
      feedback.textContent = "Codex CLI 当前不可选：需要先完成本机协作配置并附加工作区。";
      return;
    }
    feedback.textContent = "当前选择：R-Code";
  });
});

document.querySelectorAll(".provider-tab").forEach((tab) => {
  tab.addEventListener("click", () => {
    document.querySelectorAll(".provider-tab").forEach((item) => {
      const active = item === tab;
      item.classList.toggle("selected", active);
      item.setAttribute("aria-checked", String(active));
    });
    selectedProvider = tab.dataset.provider;
    selectedModel = tab.dataset.model;
    document.getElementById("provider-code").textContent = tab.dataset.code;
    document.getElementById("provider-name").textContent = selectedProvider;
    document.getElementById("provider-url").textContent = tab.dataset.url;
    document.getElementById("provider-model").textContent = selectedModel;
    document.getElementById("provider-protocol").textContent = tab.dataset.protocol;
    document.getElementById("provider-protocol-code").textContent = tab.dataset.protocolCode;
    document.getElementById("summary-provider").textContent = selectedProvider;
    document.getElementById("summary-model").textContent = selectedModel;
    savedKey = false;
    const keyField = document.getElementById("key-field");
    keyField.classList.remove("saved", "error");
    document.getElementById("api-key").value = "";
    document.getElementById("api-key").placeholder = "粘贴访问密钥";
    document.getElementById("key-feedback").textContent = "密钥只写入系统凭据库，不进入项目或配置文件。";
    document.getElementById("summary-provider").textContent = `${selectedProvider} · 密钥待保存`;
    updateReadyState();
  });
});

document.getElementById("api-key").addEventListener("input", () => {
  document.getElementById("key-field").classList.remove("error");
  document.getElementById("key-feedback").textContent = "密钥只写入系统凭据库，不进入项目或配置文件。";
});

document.getElementById("save-key").addEventListener("click", () => {
  const input = document.getElementById("api-key");
  const field = document.getElementById("key-field");
  if (!input.value.trim()) {
    field.classList.remove("saved");
    field.classList.add("error");
    document.getElementById("key-feedback").textContent = "请先填写访问密钥。";
    input.focus();
    return;
  }
  savedKey = true;
  field.classList.remove("error");
  field.classList.add("saved");
  input.value = "";
  input.placeholder = "已安全保存";
  document.getElementById("key-feedback").textContent = "原型状态：已保存。正式实现只写入系统凭据库。";
  document.getElementById("summary-provider").textContent = `${selectedProvider} · 已安全保存`;
  updateReadyState();
});

document.querySelectorAll(".scope-toggle button").forEach((option) => {
  option.addEventListener("click", () => {
    workspaceAttached = option.dataset.scope === "workspace";
    document.querySelectorAll(".scope-toggle button").forEach((item) => {
      const active = item === option;
      item.classList.toggle("selected", active);
      item.setAttribute("aria-checked", String(active));
    });
    const panel = document.getElementById("workspace-panel");
    const access = document.getElementById("access-section");
    panel.classList.toggle("hidden-scope", !workspaceAttached);
    access.classList.toggle("disabled", !workspaceAttached);
    if (workspaceAttached) {
      panel.querySelector(".workspace-path small").textContent = "示例工作区";
      panel.querySelector(".workspace-path strong").textContent = "D:\\Projects\\my-app";
      panel.querySelector(".workspace-path button").hidden = false;
      panel.querySelector(":scope > p").textContent = "所有本地路径始终限制在这个目录内。";
      document.getElementById("summary-workspace").textContent = "my-app（示例）";
      document.getElementById("summary-access").textContent = `${selectedAccess} · ${selectedAccessSummary}`;
    } else {
      panel.querySelector(".workspace-path small").textContent = "纯聊天";
      panel.querySelector(".workspace-path strong").textContent = "不附加本地工作区";
      panel.querySelector(".workspace-path button").hidden = true;
      panel.querySelector(":scope > p").textContent = "本地文件与命令工具不可用；R-Code 仍可聊天。";
      document.getElementById("summary-workspace").textContent = "未附加 · 纯聊天";
      document.getElementById("summary-access").textContent = "不适用";
    }
  });
});

document.querySelectorAll(".access-tabs button").forEach((option) => {
  option.addEventListener("click", () => {
    document.querySelectorAll(".access-tabs button").forEach((item) => {
      const active = item === option;
      item.classList.toggle("selected", active);
      item.setAttribute("aria-checked", String(active));
    });
    selectedAccess = option.dataset.mode;
    selectedAccessSummary = option.dataset.summary;
    document.getElementById("access-description").textContent = option.dataset.description;
    if (workspaceAttached) document.getElementById("summary-access").textContent = `${selectedAccess} · ${selectedAccessSummary}`;
  });
});

document.getElementById("change-workspace").addEventListener("click", () => {
  document.getElementById("change-workspace").textContent = "已选择示例";
});

document.getElementById("create-session").addEventListener("click", () => {
  if (validateBeforeCreate()) showDialog(true);
});

document.getElementById("close-button").addEventListener("click", () => showDialog(false));
document.getElementById("resume-button").addEventListener("click", hideDialog);
document.getElementById("leave-button").addEventListener("click", () => { dialogLayer.hidden = true; });

document.addEventListener("keydown", (event) => {
  if (!dialogLayer.hidden) {
    if (event.key === "Escape") hideDialog();
    return;
  }
  if (event.key === "Escape") showDialog(false);
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
  tour.style.left = `${Math.max(0, Math.min(maxLeft, moveLeft + event.clientX - moveStartX))}px`;
  tour.style.top = `${Math.max(0, Math.min(maxTop, moveTop + event.clientY - moveStartY))}px`;
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

window.focusTour = {
  setSlide,
  get current() { return current; },
  markKeySaved() {
    savedKey = true;
    document.getElementById("summary-provider").textContent = `${selectedProvider} · 已安全保存`;
    updateReadyState();
  },
};

render();
