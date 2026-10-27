import { useEffect, useState } from "react";
import { AI_MODELS, AiModel, ai, errorMessage, history } from "./api";
import { LOCALES, Locale, Theme, useSettings, useT } from "./settings";

const THEMES: Theme[] = ["system", "light", "dark"];

/** App settings: language, theme and AI. Changes apply right away. */
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
          <HistorySettings />
          <AiSettings />
        </div>
      </div>
    </div>
  );
}

/** Masking secrets in the query history, and clearing it. */
function HistorySettings() {
  const t = useT();
  const [redact, setRedact] = useState<boolean | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [note, setNote] = useState<{ text: string; error: boolean } | null>(null);

  useEffect(() => {
    history
      .settings()
      .then((settings) => setRedact(settings.redact))
      .catch((err) => setNote({ text: errorMessage(err, t), error: true }));
  }, []);

  async function changeRedact(value: boolean) {
    try {
      await history.setSettings({ redact: value });
      setRedact(value);
    } catch (err) {
      setNote({ text: errorMessage(err, t), error: true });
    }
  }

  async function clear() {
    if (!confirming) {
      setConfirming(true);
      return;
    }
    setConfirming(false);
    try {
      await history.clear(null);
      setNote({ text: t.settings.history.cleared, error: false });
    } catch (err) {
      setNote({ text: errorMessage(err, t), error: true });
    }
  }

  return (
    <fieldset className="field ai-settings">
      <legend>{t.settings.history.title}</legend>
      <label className="checkbox">
        <input
          type="checkbox"
          checked={redact ?? true}
          disabled={redact === null}
          onChange={(e) => changeRedact(e.target.checked)}
        />
        {t.settings.history.redact}
      </label>
      <p className="muted">{t.settings.history.retention}</p>
      <div className="key-row">
        <button className={confirming ? "danger" : undefined} onClick={clear} onBlur={() => setConfirming(false)}>
          {confirming ? t.settings.history.confirmClear : t.settings.history.clearAll}
        </button>
      </div>
      {note && <span className={note.error ? "error-text" : "muted"}>{note.text}</span>}
    </fieldset>
  );
}

/** The model, and the API key, which goes to the keychain and never comes back. */
function AiSettings() {
  const t = useT();
  const { settings, update } = useSettings();
  const [status, setStatus] = useState<{ saved: boolean; fromEnvironment: boolean } | null>(null);
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    ai.keyStatus()
      .then(setStatus)
      .catch((err) => setError(errorMessage(err, t)));
  }, []);

  async function change(action: () => Promise<void>) {
    setBusy(true);
    setError(null);
    try {
      await action();
      setKey("");
      setStatus(await ai.keyStatus());
    } catch (err) {
      setError(errorMessage(err, t));
    } finally {
      setBusy(false);
    }
  }

  return (
    <fieldset className="field ai-settings">
      <legend>{t.settings.ai.title}</legend>
      <label className="field">
        {t.settings.ai.model}
        <select value={settings.aiModel} onChange={(e) => update({ aiModel: e.target.value as AiModel })}>
          {AI_MODELS.map((model) => (
            <option key={model} value={model}>
              {t.settings.ai.models[model]}
            </option>
          ))}
        </select>
      </label>
      <div className="field">
        {t.settings.ai.apiKey}
        {status?.saved ? (
          <div className="key-row">
            <span className="muted">{t.settings.ai.keySaved}</span>
            <span className="spacer" />
            <button onClick={() => change(ai.forgetKey)} disabled={busy}>
              {t.settings.ai.forgetKey}
            </button>
          </div>
        ) : (
          <form
            className="key-row"
            onSubmit={(e) => {
              e.preventDefault();
              change(() => ai.setKey(key));
            }}
          >
            <input
              type="password"
              value={key}
              placeholder={t.settings.ai.keyPlaceholder}
              onChange={(e) => setKey(e.target.value)}
              autoComplete="off"
              aria-label={t.settings.ai.apiKey}
            />
            <button type="submit" disabled={busy || !key.trim()}>
              {t.settings.ai.saveKey}
            </button>
          </form>
        )}
        {status && !status.saved && status.fromEnvironment && (
          <span className="muted">{t.settings.ai.keyFromEnvironment}</span>
        )}
        {error && <span className="error-text">{error}</span>}
      </div>
      <p className="muted">{t.settings.ai.keyHelp}</p>
      <p className="muted">{t.settings.ai.privacy}</p>
    </fieldset>
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
