import { useTranslation } from "react-i18next";
import { APP_LOCALES, getAppLocale, setAppLocale, type AppLocale } from "../../i18n";

export function LanguageSettingsSection() {
  const { t } = useTranslation();
  const locale = getAppLocale();

  return (
    <section className="preference-section" id="language-block" aria-labelledby="language-heading">
      <div className="preference-section-heading">
        <div>
          <h3 id="language-heading">{t("settings.language.heading")}</h3>
          <p>{t("settings.language.description")}</p>
        </div>
      </div>
      <div className="field">
        <label htmlFor="set-interface-language">{t("settings.language.label")}</label>
        <select
          id="set-interface-language"
          className="input"
          value={locale}
          aria-label={t("settings.language.selectAria")}
          onChange={(event) => void setAppLocale(event.target.value as AppLocale)}
        >
          {APP_LOCALES.map((option) => (
            <option key={option} value={option}>
              {t(`settings.language.${option}`)}
            </option>
          ))}
        </select>
      </div>
    </section>
  );
}
