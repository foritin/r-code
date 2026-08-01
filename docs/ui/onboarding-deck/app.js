const slides = [...document.querySelectorAll(".slide")];
const progress = [...document.querySelectorAll(".progress-step")];
const chapterName = document.getElementById("chapter-name");
const stepNumber = document.getElementById("step-number");
const backButton = document.getElementById("back-button");
const nextButton = document.getElementById("next-button");
const exitDialog = document.getElementById("exit-dialog");
const resumeButton = document.getElementById("resume-button");
const leaveButton = document.getElementById("leave-button");
const keyInput = document.getElementById("api-key");
const keyField = document.getElementById("key-field");
const connectionResult = document.getElementById("connection-result");

let current = Math.max(0, Math.min(3, Number(new URLSearchParams(location.search).get("slide")) - 1 || 0));

const buttonLabels = ["开始配置", "使用所选服务", "保存并继续", "进入 R-Code"];

function render() {
  slides.forEach((slide, index) => slide.classList.toggle("active", index === current));
  progress.forEach((item, index) => {
    item.classList.toggle("active", index === current);
    item.classList.toggle("done", index < current);
    item.setAttribute("aria-current", index === current ? "step" : "false");
  });
  chapterName.textContent = slides[current].dataset.chapter;
  stepNumber.textContent = String(current + 1).padStart(2, "0");
  backButton.disabled = current === 0;
  nextButton.innerHTML = `${buttonLabels[current]} <span>→</span>`;
  slides[current].querySelector("h1")?.setAttribute("id", "slide-title");
}

function setSlide(index) {
  current = Math.max(0, Math.min(slides.length - 1, index));
  render();
}

function requestExit() {
  exitDialog.hidden = false;
  resumeButton.focus();
}

function resume() {
  exitDialog.hidden = true;
  document.getElementById("close-button").focus();
}

nextButton.addEventListener("click", () => {
  if (current === 2 && !keyInput.value.trim()) {
    keyField.classList.add("invalid");
    keyInput.focus();
    return;
  }
  if (current === slides.length - 1) {
    requestExit();
    document.querySelector("#exit-dialog .eyebrow").textContent = "SETUP COMPLETE";
    document.getElementById("exit-title").textContent = "准备好进入 R-Code 了吗？";
    leaveButton.textContent = "进入工作台";
    return;
  }
  setSlide(current + 1);
});

backButton.addEventListener("click", () => setSlide(current - 1));
progress.forEach((item) => item.addEventListener("click", () => setSlide(Number(item.dataset.go))));
document.getElementById("skip-button").addEventListener("click", requestExit);
document.getElementById("close-button").addEventListener("click", requestExit);
resumeButton.addEventListener("click", resume);
leaveButton.addEventListener("click", () => { exitDialog.hidden = true; });

document.querySelectorAll(".provider-card").forEach((card) => {
  card.addEventListener("click", () => {
    document.querySelectorAll(".provider-card").forEach((item) => {
      const selected = item === card;
      item.classList.toggle("selected", selected);
      item.setAttribute("aria-checked", String(selected));
    });
  });
});

document.getElementById("reveal-key").addEventListener("click", () => {
  keyInput.type = keyInput.type === "password" ? "text" : "password";
});

keyInput.addEventListener("input", () => keyField.classList.remove("invalid"));

document.getElementById("test-connection").addEventListener("click", () => {
  connectionResult.hidden = false;
});

document.addEventListener("keydown", (event) => {
  if (!exitDialog.hidden) {
    if (event.key === "Escape") resume();
    return;
  }
  if (event.key === "Escape") requestExit();
  if (event.key === "ArrowLeft") setSlide(current - 1);
  if (event.key === "ArrowRight") setSlide(current + 1);
});

window.onboardingDeck = { setSlide };
render();
