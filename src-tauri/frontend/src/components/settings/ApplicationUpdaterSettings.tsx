import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { errText } from "../../lib/format";
import { formatDateTimeMedium } from "../../lib/format";
import {
  onUpdaterState,
  updaterCheck,
  updaterDownload,
  updaterInstall,
  updaterRestart,
  updaterStatus,
} from "../../lib/ipc";
import type { UpdaterSnapshot } from "../../lib/updater-contract";
import "./ApplicationUpdaterSettings.css";

type BusyAction = "check" | "download" | "install" | "restart";

// 日期时间格式化唯一实现在 lib/format.ts（F-maint-04 收敛；语义不变）。
function formatDate(value: string | null, locale: string): string | null {
  return formatDateTimeMedium(value, locale);
}

function formatMegabytes(bytes: number, locale: string): string {
  return new Intl.NumberFormat(locale, {
    style: "unit",
    unit: "megabyte",
    unitDisplay: "short",
    maximumFractionDigits: 1,
  }).format(bytes / 1_048_576);
}

export function ApplicationUpdaterSettings() {
  const { t, i18n } = useTranslation();
  const [snapshot, setSnapshot] = useState<UpdaterSnapshot | null>(null);
  const [busyAction, setBusyAction] = useState<BusyAction | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [restartDeferred, setRestartDeferred] = useState(false);
  const locale = i18n.resolvedLanguage ?? i18n.language;

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;

    void updaterStatus()
      .then((status) => {
        if (!disposed) setSnapshot(status);
      })
      .catch((cause) => {
        if (!disposed) setActionError(errText(cause));
      });
    void onUpdaterState((status) => {
      if (!disposed) setSnapshot(status);
    }).then((stop) => {
      if (disposed) stop();
      else unlisten = stop;
    });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    if (snapshot?.state !== "restart_pending") setRestartDeferred(false);
  }, [snapshot?.state]);

  const backgroundError = useMemo(() => {
    if (!snapshot?.error_code) return null;
    return String(t(`errors.${snapshot.error_code}`, {
      ...snapshot.error_args,
      defaultValue: t("errors.unknown"),
    }));
  }, [snapshot?.error_args, snapshot?.error_code, t]);

  const lastCheck = formatDate(snapshot?.last_check_at ?? null, locale);
  const releaseDate = formatDate(snapshot?.release?.published_at ?? null, locale);
  const transient = snapshot != null
    && ["checking", "downloading", "installing"].includes(snapshot.state);
  const disabled = busyAction != null || transient;

  const perform = async (
    action: BusyAction,
    operation: () => Promise<UpdaterSnapshot>,
  ): Promise<UpdaterSnapshot | null> => {
    setBusyAction(action);
    setActionError(null);
    try {
      const next = await operation();
      setSnapshot(next);
      return next;
    } catch (cause) {
      setActionError(errText(cause));
      try {
        setSnapshot(await updaterStatus());
      } catch {
        // The original structured failure is the useful result. A follow-up status read is only
        // a best-effort synchronization after the command changed backend state.
      }
      return null;
    } finally {
      setBusyAction(null);
    }
  };

  const checkNow = () => perform("check", () => updaterCheck(true));
  const download = () => perform("download", updaterDownload);

  const installForLater = async () => {
    const installed = await perform("install", updaterInstall);
    if (installed?.state === "restart_pending") setRestartDeferred(true);
  };

  const installAndRestart = async () => {
    const installed = await perform("install", updaterInstall);
    if (installed?.state !== "restart_pending") return;
    setBusyAction("restart");
    setActionError(null);
    try {
      await updaterRestart();
    } catch (cause) {
      setActionError(errText(cause));
      setBusyAction(null);
    }
  };

  const restartNow = async () => {
    setBusyAction("restart");
    setActionError(null);
    try {
      await updaterRestart();
    } catch (cause) {
      setActionError(errText(cause));
      setBusyAction(null);
    }
  };

  const state = snapshot?.state ?? "idle";
  const canCheck = ["idle", "up_to_date", "available"].includes(state)
    || (state === "failed" && snapshot?.failed_operation === "check");
  const canDownload = state === "available"
    || (state === "failed" && snapshot?.failed_operation === "download");
  const canInstall = state === "downloaded"
    || (state === "failed" && snapshot?.failed_operation === "install");
  const restartPending = state === "restart_pending"
    || (state === "failed" && snapshot?.failed_operation === "restart");
  const showProgress = state === "downloading" || state === "downloaded"
    || snapshot?.progress.percent === 100;
  const stateClassName = `application-updater-state is-${state}`;
  const checkActionLabel = busyAction === "check" || state === "checking"
    ? t("settings.updater.actions.checking")
    : t("settings.updater.actions.check");
  const downloadActionLabel = busyAction === "download" || state === "downloading"
    ? t("settings.updater.actions.downloading")
    : t("settings.updater.actions.download");
  const installActionLabel = busyAction === "install" || state === "installing"
    ? t("settings.updater.actions.installing")
    : t("settings.updater.actions.installAndRestart");
  const restartActionLabel = busyAction === "restart"
    ? t("settings.updater.actions.restarting")
    : t("settings.updater.actions.restartNow");

  const releasePanel = snapshot?.release ? (
    <div className="application-updater-release">
      <div className="application-updater-release-title">
        <strong>{t("settings.updater.releaseVersion", { version: snapshot.release.version })}</strong>
        {releaseDate && <span>{t("settings.updater.releaseDate", { date: releaseDate })}</span>}
      </div>
      <div>
        <span className="application-updater-notes-label">
          {t("settings.updater.releaseNotes")}
        </span>
        <p className="application-updater-notes">
          {snapshot.release.notes || t("settings.updater.noReleaseNotes")}
        </p>
      </div>
    </div>
  ) : null;

  const progressPanel = showProgress && snapshot ? (
    <div className="application-updater-progress">
      <div>
        <span>{t("settings.updater.downloadProgress")}</span>
        <strong>
          {snapshot.progress.percent == null
            ? t("settings.updater.progressUnknown")
            : t("settings.updater.progressPercent", { percent: snapshot.progress.percent })}
        </strong>
      </div>
      <progress
        max={100}
        value={snapshot.progress.percent ?? undefined}
        aria-label={t("settings.updater.downloadProgress")}
      />
      <span>
        {snapshot.progress.total_bytes == null
          ? t("settings.updater.downloadedBytes", {
              downloaded: formatMegabytes(snapshot.progress.downloaded_bytes, locale),
            })
          : t("settings.updater.downloadedOfTotal", {
              downloaded: formatMegabytes(snapshot.progress.downloaded_bytes, locale),
              total: formatMegabytes(snapshot.progress.total_bytes, locale),
            })}
      </span>
    </div>
  ) : null;

  const checkButton = canCheck ? (
    <button type="button" className="btn" disabled={disabled} onClick={() => void checkNow()}>
      {checkActionLabel}
    </button>
  ) : null;
  const downloadButton = canDownload ? (
    <button type="button" className="btn primary" disabled={disabled} onClick={() => void download()}>
      {downloadActionLabel}
    </button>
  ) : null;
  const installButtons = canInstall ? (
    <>
      <button
        type="button"
        className="btn primary"
        disabled={disabled}
        onClick={() => void installAndRestart()}
      >
        {installActionLabel}
      </button>
      <button type="button" className="btn" disabled={disabled} onClick={() => void installForLater()}>
        {t("settings.updater.actions.installForLater")}
      </button>
    </>
  ) : null;
  const restartButtons = restartPending ? (
    <>
      <button type="button" className="btn primary" disabled={disabled} onClick={() => void restartNow()}>
        {restartActionLabel}
      </button>
      <button type="button" className="btn" disabled={disabled} onClick={() => setRestartDeferred(true)}>
        {t("settings.updater.actions.restartLater")}
      </button>
    </>
  ) : null;
  const restartDeferredNotice = restartDeferred ? (
    <div className="notebar application-updater-deferred" role="status">
      {t("settings.updater.restartDeferred")}
    </div>
  ) : null;
  const errorNotice = actionError || backgroundError ? (
    <div className="errbar application-updater-error" role="alert">
      {actionError ?? backgroundError}
    </div>
  ) : null;

  return (
    <section
      className="preference-section application-updater"
      id="application-updater-block"
      aria-labelledby="application-updater-heading"
    >
      <div className="preference-section-heading">
        <div>
          <h3 id="application-updater-heading">{t("settings.updater.heading")}</h3>
          <p>{t("settings.updater.description")}</p>
        </div>
      </div>

      <div className="application-updater-summary">
        <div>
          <span>{t("settings.updater.currentVersion")}</span>
          <strong>{snapshot?.current_version ?? t("settings.updater.loadingVersion")}</strong>
        </div>
        <div>
          <span>{t("settings.updater.statusLabel")}</span>
          <strong className={stateClassName} role="status" aria-live="polite">
            {t(`settings.updater.states.${state}`)}
          </strong>
        </div>
        <div>
          <span>{t("settings.updater.lastChecked")}</span>
          <strong>{lastCheck ?? t("settings.updater.neverChecked")}</strong>
        </div>
      </div>

      {releasePanel}
      {progressPanel}

      <div className="application-updater-actions">
        {checkButton}
        {downloadButton}
        {installButtons}
        {restartButtons}
      </div>

      {restartDeferredNotice}
      {errorNotice}
      <p className="application-updater-safety">{t("settings.updater.safetyNotice")}</p>
    </section>
  );
}
