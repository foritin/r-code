const tauri = window.__TAURI__;
const isTauri = Boolean(tauri?.core?.invoke);
const localListeners = new Map();

const previewInfo = {
  version: "0.1.0",
  defaultInstallPath: "C:\\Users\\你\\AppData\\Local\\R-Code",
  existingInstall: false,
  packageSizeMb: 18,
};

const bridge = {
  async invoke(command, payload = {}) {
    if (isTauri) return tauri.core.invoke(command, payload);
    if (command === "installer_info") return previewInfo;
    if (command === "choose_directory") return "D:\\Apps\\R-Code";
    if (command === "legal_document") {
      return payload.kind === "license"
        ? "MIT License\n\nCopyright (c) 2026 R-Code Team\n\nPermission is hereby granted, free of charge, to any person obtaining a copy..."
        : "R-Code Privacy Notice\n\nR-Code stores tasks, sessions and settings locally. Model requests are sent only to the provider configured by the user.";
    }
    if (command === "start_install") {
      runPreviewInstall(payload.request?.installPath ?? previewInfo.defaultInstallPath);
      return null;
    }
    if (command === "cancel_install") return { accepted: true, message: "正在停止安装并清理临时文件" };
    return null;
  },
  async listen(name, handler) {
    if (isTauri) return tauri.event.listen(name, handler);
    localListeners.set(name, handler);
    return () => localListeners.delete(name);
  },
};

const elements = {
  views: [...document.querySelectorAll(".view")],
  titleVersion: document.querySelector("#title-version"),
  welcomeVersion: document.querySelector("#welcome-version"),
  sharedVersions: [...document.querySelectorAll(".shared-version")],
  completeVersion: document.querySelector("#complete-version"),
  welcomeLede: document.querySelector("#welcome-lede"),
  existingNote: document.querySelector("#existing-note"),
  customize: document.querySelector("#customize"),
  customPanel: document.querySelector("#custom-panel"),
  installPath: document.querySelector("#install-path"),
  pathSummary: document.querySelector("#path-summary"),
  browsePath: document.querySelector("#browse-path"),
  createShortcuts: document.querySelector("#create-shortcuts"),
  launchAfter: document.querySelector("#launch-after"),
  installNow: document.querySelector("#install-now"),
  installingPath: document.querySelector("#installing-path"),
  installingMessage: document.querySelector("#installing-message"),
  progressValue: document.querySelector("#progress-value"),
  progressTrack: document.querySelector("#progress-track"),
  progressFill: document.querySelector("#progress-fill"),
  progressNote: document.querySelector("#progress-note"),
  phases: [...document.querySelectorAll(".phase")],
  cancelInstall: document.querySelector("#cancel-install"),
  completePath: document.querySelector("#complete-path"),
  completeLater: document.querySelector("#complete-later"),
  completePrimary: document.querySelector("#complete-primary"),
  errorCode: document.querySelector("#error-code"),
  errorMessage: document.querySelector("#error-message"),
  errorDetail: document.querySelector("#error-detail"),
  errorClose: document.querySelector("#error-close"),
  retryInstall: document.querySelector("#retry-install"),
  decisionBackdrop: document.querySelector("#decision-backdrop"),
  decisionTitle: document.querySelector("#decision-title"),
  decisionCopy: document.querySelector("#decision-copy"),
  continueInstall: document.querySelector("#continue-install"),
  confirmCancel: document.querySelector("#confirm-cancel"),
  documentBackdrop: document.querySelector("#document-backdrop"),
  documentTitle: document.querySelector("#document-title"),
  documentContent: document.querySelector("#document-content"),
  closeDocument: document.querySelector("#close-document"),
  documentDone: document.querySelector("#document-done"),
  toast: document.querySelector("#toast"),
  minimizeWindow: document.querySelector("#minimize-window"),
  closeWindow: document.querySelector("#close-window"),
};

let info = previewInfo;
let currentView = "welcome";
let installActive = false;
let cancelable = true;
let closeAfterCancel = false;
let modalReturnFocus = null;
let toastTimer = null;

function setView(name) {
  currentView = name;
  for (const view of elements.views) {
    const active = view.dataset.view === name;
    view.classList.toggle("active", active);
    view.setAttribute("aria-hidden", String(!active));
  }
  const heading = document.querySelector(`[data-view="${name}"] h2`);
  requestAnimationFrame(() => heading?.focus?.({ preventScroll: true }));
}

function setVersion(version) {
  const short = version.startsWith("v") ? version : `v${version}`;
  elements.titleVersion.textContent = short;
  elements.welcomeVersion.textContent = short;
  elements.sharedVersions.forEach((node) => { node.textContent = short; });
  elements.completeVersion.textContent = short.slice(1);
}

function updatePath(path) {
  elements.installPath.value = path;
  elements.pathSummary.textContent = path;
  elements.installingPath.textContent = path;
}

function updatePhases(stage) {
  const activeIndex = stage === "extracting" ? 0 : stage === "finalizing" ? 2 : 1;
  elements.phases.forEach((phase, index) => {
    const state = phase.querySelector(".phase-state");
    phase.classList.toggle("done", index < activeIndex);
    phase.classList.toggle("active", index === activeIndex);
    if (index < activeIndex) {
      phase.querySelector(".phase-dot").textContent = "✓";
      state.textContent = "完成";
    } else if (index === activeIndex) {
      phase.querySelector(".phase-dot").textContent = String(index + 1);
      state.textContent = "进行中";
    } else {
      phase.querySelector(".phase-dot").textContent = String(index + 1);
      state.textContent = "等待";
    }
  });
}

function updateProgress(payload) {
  const percent = Math.max(0, Math.min(100, Number(payload.percent) || 0));
  elements.progressValue.textContent = `${percent}%`;
  elements.progressFill.style.width = `${percent}%`;
  elements.progressTrack.setAttribute("aria-valuenow", String(percent));
  elements.installingMessage.textContent = payload.message;
  cancelable = Boolean(payload.cancelable);
  elements.cancelInstall.disabled = !cancelable;
  elements.cancelInstall.textContent = cancelable ? "取消安装" : "正在安全写入";
  elements.progressNote.textContent = cancelable
    ? "验证阶段可以安全取消；写入开始后会完整结束当前操作。"
    : "正在执行关键写入。为避免损坏安装，当前阶段不能强制中止。";
  updatePhases(payload.stage);
}

function handleProgress(payload) {
  if (!payload) return;
  if (["extracting", "preparing", "installing", "finalizing"].includes(payload.stage)) {
    installActive = true;
    if (currentView !== "installing") setView("installing");
    updateProgress(payload);
    return;
  }

  installActive = false;
  cancelable = false;
  closeDecisionModal();
  if (payload.stage === "complete") {
    elements.completePath.textContent = payload.installPath || elements.installPath.value;
    const shouldLaunch = elements.launchAfter.checked;
    elements.completePrimary.innerHTML = shouldLaunch ? "启动 R-Code <span aria-hidden=\"true\">→</span>" : "完成";
    elements.completeLater.hidden = !shouldLaunch;
    setView("complete");
  } else if (payload.stage === "cancelled") {
    if (closeAfterCancel) {
      bridge.invoke("close_window");
    } else {
      setView("welcome");
      showToast(payload.message || "安装已取消");
    }
    closeAfterCancel = false;
  } else if (payload.stage === "error") {
    showError(payload.errorCode || "RCI-000", payload.message || "安装没有完成");
  }
}

function showError(code, message) {
  installActive = false;
  elements.errorCode.textContent = code;
  elements.errorMessage.textContent = message;
  elements.errorDetail.textContent = "应用数据没有被安装器主动删除。请检查目标目录权限后重试。";
  setView("error");
}

function showToast(message) {
  clearTimeout(toastTimer);
  elements.toast.textContent = message;
  elements.toast.classList.add("show");
  toastTimer = setTimeout(() => elements.toast.classList.remove("show"), 3200);
}

function setModalBackgroundInert(value) {
  document.querySelector(".layout").inert = value;
  document.querySelector(".window-actions").inert = value;
}

function openDecisionModal(canCancel, fromClose = false) {
  modalReturnFocus = document.activeElement;
  closeAfterCancel = fromClose;
  elements.continueInstall.hidden = false;
  elements.confirmCancel.disabled = false;
  if (canCancel) {
    elements.decisionTitle.textContent = "取消安装？";
    elements.decisionCopy.textContent = "现在取消会停止准备过程并清理临时文件。已经存在的项目和设置不会被删除。";
    elements.confirmCancel.hidden = false;
    elements.continueInstall.textContent = "继续安装";
  } else {
    elements.decisionTitle.textContent = "正在完成关键写入";
    elements.decisionCopy.textContent = "为了避免出现半安装状态，应用文件写入期间不能强制退出。完成后即可关闭安装程序。";
    elements.confirmCancel.hidden = true;
    elements.continueInstall.textContent = "继续等待";
  }
  elements.decisionBackdrop.classList.add("open");
  elements.decisionBackdrop.setAttribute("aria-hidden", "false");
  setModalBackgroundInert(true);
  (elements.confirmCancel.hidden ? elements.continueInstall : elements.continueInstall).focus();
}

function closeDecisionModal() {
  if (!elements.decisionBackdrop.classList.contains("open")) return;
  elements.decisionBackdrop.classList.remove("open");
  elements.decisionBackdrop.setAttribute("aria-hidden", "true");
  setModalBackgroundInert(false);
  modalReturnFocus?.focus?.();
  modalReturnFocus = null;
}

async function openDocument(kind, trigger) {
  modalReturnFocus = trigger;
  elements.documentTitle.textContent = kind === "license" ? "软件许可" : "隐私说明";
  elements.documentContent.textContent = "正在加载…";
  elements.documentBackdrop.classList.add("open");
  elements.documentBackdrop.setAttribute("aria-hidden", "false");
  setModalBackgroundInert(true);
  elements.closeDocument.focus();
  try {
    elements.documentContent.textContent = await bridge.invoke("legal_document", { kind });
  } catch (error) {
    elements.documentContent.textContent = String(error);
  }
}

function closeDocument() {
  elements.documentBackdrop.classList.remove("open");
  elements.documentBackdrop.setAttribute("aria-hidden", "true");
  setModalBackgroundInert(false);
  modalReturnFocus?.focus?.();
  modalReturnFocus = null;
}

function runPreviewInstall(path) {
  const events = [
    [100, { stage: "extracting", percent: 18, message: "正在校验并准备安装组件", cancelable: true }],
    [650, { stage: "preparing", percent: 32, message: "安装组件已就绪，准备写入应用文件", cancelable: false }],
    [1150, { stage: "installing", percent: 62, message: "正在写入应用文件和卸载组件", cancelable: false }],
    [1750, { stage: "finalizing", percent: 90, message: "正在创建快捷方式并完成系统集成", cancelable: false }],
    [2300, { stage: "complete", percent: 100, message: "R-Code 已安装完成", cancelable: false, installPath: path }],
  ];
  for (const [delay, payload] of events) {
    setTimeout(() => localListeners.get("installer-progress")?.({ payload }), delay);
  }
}

elements.customize.addEventListener("click", () => {
  const open = elements.customPanel.classList.toggle("open");
  elements.customize.setAttribute("aria-expanded", String(open));
  elements.customize.textContent = open ? "收起" : "自定义";
  if (open) elements.installPath.focus();
});

elements.installPath.addEventListener("input", () => {
  elements.pathSummary.textContent = elements.installPath.value.trim() || "请选择安装位置";
});

elements.browsePath.addEventListener("click", async () => {
  elements.browsePath.disabled = true;
  try {
    const selected = await bridge.invoke("choose_directory", { current: elements.installPath.value });
    if (selected) updatePath(selected);
  } catch (error) {
    showToast(String(error));
  } finally {
    elements.browsePath.disabled = false;
  }
});

elements.installNow.addEventListener("click", async () => {
  const installPath = elements.installPath.value.trim();
  if (!installPath) {
    elements.customPanel.classList.add("open");
    elements.customize.setAttribute("aria-expanded", "true");
    elements.installPath.focus();
    showToast("请先选择安装位置");
    return;
  }
  updatePath(installPath);
  installActive = true;
  cancelable = true;
  setView("installing");
  updateProgress({ stage: "extracting", percent: 2, message: "正在验证安装包", cancelable: true });
  try {
    await bridge.invoke("start_install", {
      request: {
        installPath,
        createShortcuts: elements.createShortcuts.checked,
      },
    });
  } catch (error) {
    showError("RCI-100", String(error));
  }
});

elements.cancelInstall.addEventListener("click", () => openDecisionModal(cancelable, false));
elements.continueInstall.addEventListener("click", closeDecisionModal);
elements.confirmCancel.addEventListener("click", async () => {
  elements.confirmCancel.disabled = true;
  try {
    const response = await bridge.invoke("cancel_install");
    if (!response.accepted) {
      closeDecisionModal();
      openDecisionModal(false, closeAfterCancel);
      showToast(response.message);
      return;
    }
    elements.decisionCopy.textContent = response.message;
    elements.confirmCancel.hidden = true;
    elements.continueInstall.hidden = true;
  } catch (error) {
    showToast(String(error));
    elements.confirmCancel.disabled = false;
  }
});

elements.completePrimary.addEventListener("click", async () => {
  if (!elements.launchAfter.checked) {
    await bridge.invoke("close_window");
    return;
  }
  elements.completePrimary.disabled = true;
  try {
    await bridge.invoke("launch_installed_app");
  } catch (error) {
    elements.completePrimary.disabled = false;
    showError("RCI-108", String(error));
  }
});
elements.completeLater.addEventListener("click", () => bridge.invoke("close_window"));
elements.errorClose.addEventListener("click", () => bridge.invoke("close_window"));
elements.retryInstall.addEventListener("click", () => setView("welcome"));

document.querySelectorAll("[data-document]").forEach((button) => {
  button.addEventListener("click", () => openDocument(button.dataset.document, button));
});
elements.closeDocument.addEventListener("click", closeDocument);
elements.documentDone.addEventListener("click", closeDocument);

elements.minimizeWindow.addEventListener("click", () => bridge.invoke("minimize_window"));
elements.closeWindow.addEventListener("click", () => bridge.invoke("close_window"));

document.addEventListener("keydown", (event) => {
  const activeModal = elements.documentBackdrop.classList.contains("open")
    ? elements.documentBackdrop
    : elements.decisionBackdrop.classList.contains("open")
      ? elements.decisionBackdrop
      : null;
  if (event.key === "Tab" && activeModal) {
    const focusable = [...activeModal.querySelectorAll("button:not([disabled]):not([hidden]), [tabindex=\"0\"]")]
      .filter((node) => node.getClientRects().length > 0);
    if (focusable.length) {
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    }
    return;
  }
  if (event.key !== "Escape") return;
  if (elements.documentBackdrop.classList.contains("open")) {
    event.preventDefault();
    closeDocument();
  } else if (elements.decisionBackdrop.classList.contains("open")) {
    event.preventDefault();
    closeDecisionModal();
  } else if (currentView === "welcome" && elements.customPanel.classList.contains("open")) {
    elements.customize.click();
  }
});

bridge.listen("installer-progress", (event) => handleProgress(event.payload));
bridge.listen("installer-close-requested", (event) => {
  openDecisionModal(Boolean(event.payload?.cancelable), true);
});

(async function initialize() {
  try {
    info = await bridge.invoke("installer_info");
    setVersion(info.version);
    updatePath(info.defaultInstallPath);
    elements.existingNote.classList.toggle("show", Boolean(info.existingInstall));
    if (info.existingInstall) {
      elements.welcomeLede.textContent = "更新当前用户安装，不需要管理员权限。";
      elements.installNow.innerHTML = "更新 R-Code <span aria-hidden=\"true\">→</span>";
    }
  } catch (error) {
    showError("RCI-099", `无法初始化安装程序：${error}`);
  }
})();
