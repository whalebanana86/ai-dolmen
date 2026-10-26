import { LOCALES, Locale, Theme, useSettings, useT } from "./settings";

const THEMES: Theme[] = ["system", "light", "dark"];

/** App settings: language and theme. Changes apply right away. */
export function SettingsDialog({ onClose }: { onClose: () => void }) {
  const t = useT();
  const { settings, update } = useSettings();

  return (
    <div className="dialog-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="dialog settings-dialog" role="dialog" aria-label={t.settings.title}>
        <header className="side-panel-header">
          <strong>{t.settings.title}</strong>
          <span className="spacer" />
          <button onClick={onClose} title={t.common.close}>
            ✕
          </button>
        </header>
        <div className="dialog-body">
          <label className="field">
            {t.settings.language}
            <select value={settings.locale} onChange={(e) => update({ locale: e.target.value as Locale })}>
              {LOCALES.map((locale) => (
                <option key={locale.value} value={locale.value}>
                  {locale.name}
                </option>
              ))}
            </select>
          </label>
          <fieldset className="field theme-choice">
            <legend>{t.settings.theme}</legend>
            {THEMES.map((theme) => (
              <label key={theme} className="checkbox">
                <input
                  type="radio"
                  name="theme"
                  checked={settings.theme === theme}
                  onChange={() => update({ theme })}
                />
                {t.settings.themes[theme]}
              </label>
            ))}
          </fieldset>
        </div>
      </div>
    </div>
  );
}

/** The ⚙ button that opens the settings. */
export function SettingsButton({ onClick }: { onClick: () => void }) {
  const t = useT();
  return (
    <button className="icon-button" onClick={onClick} title={t.common.settings} aria-label={t.common.settings}>
      ⚙
    </button>
  );
}
