import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { errText } from "../../lib/format";
import {
  nativeNotificationPermissionState,
  nativeNotificationRequestPermission,
} from "../../lib/ipc";
import type { NativeNotificationPermissionState } from "../../lib/types";

export function NativeNotificationSettings() {
  const { t } = useTranslation();
  const [permission, setPermission] = useState<NativeNotificationPermissionState | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      setPermission(await nativeNotificationPermissionState());
    } catch (cause) {
      setPermission("unavailable");
      setError(errText(cause));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    void nativeNotificationPermissionState()
      .then((state) => {
        if (!cancelled) setPermission(state);
      })
      .catch((cause) => {
        if (cancelled) return;
        setPermission("unavailable");
        setError(errText(cause));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const requestPermission = async () => {
    setBusy(true);
    setError(null);
    try {
      setPermission(await nativeNotificationRequestPermission());
    } catch (cause) {
      setError(errText(cause));
      // Re-read after an error: some platforms can persist the user's choice even when the
      // plugin reports that the prompt itself could not be presented cleanly.
      try {
        setPermission(await nativeNotificationPermissionState());
      } catch {
        setPermission("unavailable");
      }
    } finally {
      setBusy(false);
    }
  };

  const state = permission ?? "loading";
  const canRequest = permission === "prompt" || permission === "denied";

  return (
    <section
      className="preference-section"
      id="native-notifications-block"
      aria-labelledby="native-notifications-heading"
    >
      <div className="preference-section-heading">
        <div>
          <h3 id="native-notifications-heading">{t("settings.notifications.heading")}</h3>
          <p>{t("settings.notifications.description")}</p>
        </div>
      </div>

      <div className="native-notification-permission">
        <div
          className={`native-notification-state is-${state}`}
          role="status"
          aria-live="polite"
        >
          <span className="native-notification-state-dot" aria-hidden="true" />
          <span className="native-notification-state-copy">
            <strong>{t(`settings.notifications.status.${state}.label`)}</strong>
            <span>{t(`settings.notifications.status.${state}.description`)}</span>
          </span>
        </div>

        {canRequest && (
          <button
            type="button"
            className="btn primary"
            disabled={busy}
            onClick={() => void requestPermission()}
          >
            {busy
              ? t("settings.notifications.requestingPermission")
              : t("settings.notifications.requestPermission")}
          </button>
        )}
        {permission === "unavailable" && (
          <button type="button" className="btn" disabled={busy} onClick={() => void refresh()}>
            {busy
              ? t("settings.notifications.checkingPermission")
              : t("settings.notifications.checkAgain")}
          </button>
        )}
      </div>

      {error && (
        <div className="errbar native-notification-error" role="alert">
          {t("settings.notifications.permissionError", { detail: error })}
        </div>
      )}
    </section>
  );
}
