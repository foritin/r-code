const tour = document.querySelector(".tour");
const header = document.querySelector(".tour-header");
const viewport = document.getElementById("viewport");
const track = document.getElementById("track");
const slides = [...document.querySelectorAll(".slide")];
const dots = [...document.querySelectorAll(".dot")];
const back = document.getElementById("back");
const next = document.getElementById("next");
const dialog = document.getElementById("dialog");

const params = new URLSearchParams(location.search);
let current = Math.max(0, Math.min(slides.length - 1, Number(params.get("slide")) - 1 || 0));
let keySaved = false;
let provider = "DeepSeek";
let model = "deepseek-v4-flash";
let workspace = "my-app";
let access = "替我审批";
let accessShort = "中高风险再问";

let slideDragging = false;
let slidePointer = null;
let slideStartX = 0;
let slideDelta = 0;
let slideStartTime = 0;

function offset(index = current, delta = 0) {
  return -(index * viewport.clientWidth) + delta;
}

function moveTrack(delta = 0, animate = true) {
  track.classList.toggle("dragging", !animate);
  track.style.transform = `translate3d(${offset(current, delta)}px,0,0)`;
}

function updateLaunch() {
  document.getElementById("launch-line").textContent = `R-Code × ${provider} × ${workspace}`;
  document.getElementById("launch-access").textContent = workspace === "纯聊天" ? "不附加工作区" : `${access} · ${accessShort}`;
  document.querySelector(".launch-copy > span").textContent = keySaved ? "READY" : "CHECK";
  document.getElementById("launch-title").textContent = keySaved ? "开工。" : "差一步。";
  document.getElementById("create-session").innerHTML = keySaved
    ? "<strong>创建会话</strong><i>→</i>"
    : "<strong>补全并创建</strong><i>→</i>";
}

function render({ focus = false } = {}) {
  slides.forEach((slide, index) => slide.setAttribute("aria-hidden", String(index !== current)));
  dots.forEach((dot, index) => {
    dot.classList.toggle("active", index === current);
    dot.setAttribute("aria-current", index === current ? "step" : "false");
  });
  document.getElementById("step-name").textContent = slides[current].dataset.label;
  document.getElementById("step-current").textContent = String(current + 1).padStart(2, "0");
  back.disabled = current === 0;
  next.innerHTML = current === slides.length - 1 ? "<i>完成</i><span>→</span>" : "<i>下一页</i><span>→</span>";
  updateLaunch();
  moveTrack(0, true);
  if (focus) viewport.focus({ preventScroll: true });
}

function setSlide(index, options = {}) {
  current = Math.max(0, Math.min(slides.length - 1, index));
  render(options);
}

function validateCreate() {
  if (keySaved) return true;
  setSlide(2);
  document.getElementById("secret-field").classList.add("error");
  document.getElementById("secret-note").textContent = "先保存密钥。";
  document.getElementById("api-key").focus();
  return false;
}

function goNext() {
  if (current === slides.length - 1) {
    if (validateCreate()) showDialog(true);
  } else {
    setSlide(current + 1, { focus: true });
  }
}

function showDialog(done = false) {
  dialog.hidden = false;
  document.getElementById("dialog-mark").textContent = done ? "READY" : "PAUSE";
  document.getElementById("dialog-title").textContent = done ? "配置已确认。" : "稍后设置？";
  document.getElementById("dialog-copy").textContent = done ? "原型不写配置。" : "可从设置继续。";
  document.getElementById("resume").focus();
}

function hideDialog() {
  dialog.hidden = true;
  viewport.focus({ preventScroll: true });
}

function interactive(target) {
  return Boolean(target.closest("button,input,label,a,textarea,select"));
}

viewport.addEventListener("pointerdown", (event) => {
  if (event.button !== 0 || interactive(event.target)) return;
  slideDragging = true;
  slidePointer = event.pointerId;
  slideStartX = event.clientX;
  slideStartTime = performance.now();
  slideDelta = 0;
  viewport.classList.add("dragging");
  viewport.setPointerCapture(slidePointer);
  moveTrack(0, false);
});

viewport.addEventListener("pointermove", (event) => {
  if (!slideDragging || event.pointerId !== slidePointer) return;
  slideDelta = event.clientX - slideStartX;
  if ((current === 0 && slideDelta > 0) || (current === slides.length - 1 && slideDelta < 0)) slideDelta *= .25;
  moveTrack(slideDelta, false);
});

function endSlide(event) {
  if (!slideDragging || event.pointerId !== slidePointer) return;
  const time = Math.max(1, performance.now() - slideStartTime);
  const speed = slideDelta / time;
  const threshold = Math.min(120, viewport.clientWidth * .12);
  slideDragging = false;
  viewport.classList.remove("dragging");
  try { viewport.releasePointerCapture(slidePointer); } catch (_) {}
  slidePointer = null;
  if ((slideDelta < -threshold || speed < -.55) && current < slides.length - 1) current += 1;
  else if ((slideDelta > threshold || speed > .55) && current > 0) current -= 1;
  slideDelta = 0;
  render();
}

viewport.addEventListener("pointerup", endSlide);
viewport.addEventListener("pointercancel", endSlide);

dots.forEach((dot) => dot.addEventListener("click", () => setSlide(Number(dot.dataset.go), { focus: true })));
back.addEventListener("click", () => setSlide(current - 1, { focus: true }));
next.addEventListener("click", goNext);

document.querySelectorAll(".engine-option").forEach((option) => {
  option.addEventListener("click", () => {
    document.getElementById("engine-note").textContent = option.dataset.engine === "codex"
      ? "先连接 Codex CLI 并附加工作区。"
      : "R-Code 可直接聊天。";
  });
});

document.querySelectorAll(".provider-pick button").forEach((option) => {
  option.addEventListener("click", () => {
    document.querySelectorAll(".provider-pick button").forEach((item) => {
      const selected = item === option;
      item.classList.toggle("selected", selected);
      item.setAttribute("aria-checked", String(selected));
    });
    provider = option.dataset.provider;
    model = option.dataset.model;
    document.getElementById("provider-code").textContent = option.dataset.code;
    document.getElementById("provider-name").textContent = provider;
    document.getElementById("provider-model").textContent = model;
    document.getElementById("provider-url").textContent = option.dataset.url;
    keySaved = false;
    const input = document.getElementById("api-key");
    input.value = "";
    input.placeholder = "粘贴密钥";
    document.getElementById("secret-field").classList.remove("error", "saved");
    document.getElementById("secret-note").textContent = "只进系统凭据库。";
    updateLaunch();
  });
});

document.getElementById("api-key").addEventListener("input", () => {
  document.getElementById("secret-field").classList.remove("error");
  document.getElementById("secret-note").textContent = "只进系统凭据库。";
});

document.getElementById("save-key").addEventListener("click", () => {
  const input = document.getElementById("api-key");
  const field = document.getElementById("secret-field");
  if (!input.value.trim()) {
    field.classList.remove("saved");
    field.classList.add("error");
    document.getElementById("secret-note").textContent = "先粘贴密钥。";
    input.focus();
    return;
  }
  keySaved = true;
  field.classList.remove("error");
  field.classList.add("saved");
  input.value = "";
  input.placeholder = "已保存";
  document.getElementById("secret-note").textContent = "已安全保存。";
  updateLaunch();
});

document.querySelectorAll(".scope-mode button").forEach((option) => {
  option.addEventListener("click", () => {
    const attached = option.dataset.scope === "workspace";
    document.querySelectorAll(".scope-mode button").forEach((item) => {
      const selected = item === option;
      item.classList.toggle("selected", selected);
      item.setAttribute("aria-checked", String(selected));
    });
    document.querySelector(".scope-controls").classList.toggle("chat-only", !attached);
    workspace = attached ? "my-app" : "纯聊天";
    document.querySelector(".workspace-object > span").textContent = attached ? "示例工作区" : "无工作区";
    document.getElementById("workspace-value").textContent = attached ? "D:\\Projects\\my-app" : "本地工具不可用";
    document.getElementById("change-workspace").hidden = !attached;
    document.getElementById("access-note").textContent = attached ? `${accessShort} · R4 拒绝` : "R-Code 仍可聊天";
    updateLaunch();
  });
});

document.querySelectorAll(".access-pick button").forEach((option) => {
  option.addEventListener("click", () => {
    document.querySelectorAll(".access-pick button").forEach((item) => {
      const selected = item === option;
      item.classList.toggle("selected", selected);
      item.setAttribute("aria-checked", String(selected));
    });
    access = option.dataset.mode;
    accessShort = option.dataset.short;
    document.getElementById("access-note").textContent = `${accessShort} · R4 拒绝`;
    updateLaunch();
  });
});

document.getElementById("change-workspace").addEventListener("click", () => {
  document.getElementById("change-workspace").textContent = "已选示例";
});

document.getElementById("create-session").addEventListener("click", () => {
  if (validateCreate()) showDialog(true);
});
document.getElementById("close").addEventListener("click", () => showDialog(false));
document.getElementById("resume").addEventListener("click", hideDialog);
document.getElementById("leave").addEventListener("click", () => { dialog.hidden = true; });

document.addEventListener("keydown", (event) => {
  if (!dialog.hidden) {
    if (event.key === "Escape") hideDialog();
    return;
  }
  if (event.key === "Escape") showDialog(false);
  if (event.key === "ArrowRight") setSlide(current + 1, { focus: true });
  if (event.key === "ArrowLeft") setSlide(current - 1, { focus: true });
});

let moving = false;
let movePointer = null;
let moveX = 0;
let moveY = 0;
let moveLeft = 0;
let moveTop = 0;

header.addEventListener("pointerdown", (event) => {
  if (event.button !== 0 || interactive(event.target)) return;
  const rect = tour.getBoundingClientRect();
  moving = true;
  movePointer = event.pointerId;
  moveX = event.clientX;
  moveY = event.clientY;
  moveLeft = rect.left;
  moveTop = rect.top;
  tour.style.left = `${rect.left}px`;
  tour.style.top = `${rect.top}px`;
  tour.style.transform = "none";
  header.setPointerCapture(movePointer);
  header.classList.add("moving");
});

header.addEventListener("pointermove", (event) => {
  if (!moving || event.pointerId !== movePointer) return;
  const maxLeft = Math.max(0, innerWidth - tour.offsetWidth);
  const maxTop = Math.max(0, innerHeight - tour.offsetHeight);
  tour.style.left = `${Math.max(0, Math.min(maxLeft, moveLeft + event.clientX - moveX))}px`;
  tour.style.top = `${Math.max(0, Math.min(maxTop, moveTop + event.clientY - moveY))}px`;
});

function endMove(event) {
  if (!moving || event.pointerId !== movePointer) return;
  moving = false;
  header.classList.remove("moving");
  try { header.releasePointerCapture(movePointer); } catch (_) {}
  movePointer = null;
}

header.addEventListener("pointerup", endMove);
header.addEventListener("pointercancel", endMove);

window.addEventListener("resize", () => moveTrack(0, false));

window.campaignTour = {
  setSlide,
  get current() { return current; },
  markReady() { keySaved = true; updateLaunch(); },
};

render();
