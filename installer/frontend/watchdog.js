(() => {
  "use strict";

  if (document.documentElement.dataset.installerBooted === "true") return;

  const tauriApi = window.__TAURI__;
  const pathSummary = document.querySelector("#path-summary");
  const installButton = document.querySelector("#install-now");
  const closeButton = document.querySelector("#close-window");
  const minimizeButton = document.querySelector("#minimize-window");

  if (pathSummary) pathSummary.textContent = "安装器初始化失败，请重新下载安装包";
  if (installButton) {
    installButton.disabled = true;
    installButton.textContent = "安装器不可用";
  }

  minimizeButton?.addEventListener("click", () => {
    tauriApi?.core?.invoke("minimize_window").catch(() => {});
  });
  closeButton?.addEventListener("click", () => {
    tauriApi?.core?.invoke("close_window").catch(() => window.close());
  });

  document.documentElement.dataset.installerReady = "error";
  console.error("R-Code installer application script did not finish booting");
})();
