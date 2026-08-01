export const ONBOARDING_OPEN_EVENT = "r-code:onboarding:open";
export const RUNTIME_SETTINGS_CHANGED_EVENT = "r-code:runtime-settings-changed";

const ONBOARDING_STORAGE_KEY = "r-code.onboarding.campaign.v1";

type OnboardingOutcome = "completed" | "dismissed";

interface OnboardingReceipt {
  outcome: OnboardingOutcome;
  savedAt: string;
}

/**
 * `?onboarding=1` is a deliberate QA/deep-link override. `?onboarding=0` keeps
 * screenshots and embedded demos deterministic without mutating the receipt.
 */
export function shouldOpenOnboarding(): boolean {
  const override = new URLSearchParams(window.location.search).get("onboarding");
  if (override === "1") return true;
  if (override === "0") return false;
  // Browser demos and viewport tests have no install lifecycle. They opt in with
  // the QA override above; the real first-run decision belongs to the Tauri app.
  if (!Reflect.has(window, "__TAURI_INTERNALS__")) return false;
  try {
    return window.localStorage.getItem(ONBOARDING_STORAGE_KEY) == null;
  } catch {
    return true;
  }
}

export function saveOnboardingReceipt(outcome: OnboardingOutcome): void {
  const receipt: OnboardingReceipt = { outcome, savedAt: new Date().toISOString() };
  try {
    window.localStorage.setItem(ONBOARDING_STORAGE_KEY, JSON.stringify(receipt));
  } catch {
    // A restricted WebView may reject localStorage. Finishing the current flow
    // still succeeds; the tour can appear again on the next launch.
  }
}

export function requestOnboarding(): void {
  window.dispatchEvent(new Event(ONBOARDING_OPEN_EVENT));
}

export function announceRuntimeSettingsChanged(): void {
  window.dispatchEvent(new Event(RUNTIME_SETTINGS_CHANGED_EVENT));
}
