import { FormEvent, useEffect, useState } from "react";
import {
  ENVIRONMENTS,
  Environment,
  PasswordChange,
  SavedConnection,
  SessionInfo,
  SslMode,
  appVersion,
  connections,
  errorMessage,
  isLocalHost,
  pg,
} from "./api";
import { SettingsButton, SettingsDialog } from "./SettingsDialog";
import { useT } from "./settings";

const SSL_MODES: SslMode[] = ["disable", "prefer", "require", "verify-full"];

/** The form fields; the port stays a string while it is being edited. */
type Draft = Omit<SavedConnection, "port"> & { port: string };

const NEW_CONNECTION: Draft = {
  id: "",
  name: "",
  host: "localhost",
  port: "5432",
  user: "postgres",
  database: "postgres",
  sslMode: "prefer",
  hasPassword: false,
  snapshotDir: null,
  snapshotRemote: null,
  recordHistory: true,
  allowSnapshotApply: null,
  environment: null,
  readOnly: null,
};

function toDraft(connection: SavedConnection): Draft {
  return { ...connection, port: String(connection.port) };
}

function defaultName(draft: Draft) {
  return `${draft.database} @ ${draft.host}`;
}

interface Props {
  /** Gets the saved connection the session came from, or null for an unsaved one. */
  onConnected: (session: SessionInfo, connection: SavedConnection | null) => void;
}

export function ConnectionForm({ onConnected }: Props) {
  const t = useT();
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [version, setVersion] = useState("");
  const [saved, setSaved] = useState<SavedConnection[]>([]);
  const [draft, setDraft] = useState<Draft>(NEW_CONNECTION);
  const [sslChosen, setSslChosen] = useState(false);
  const [password, setPassword] = useState("");
  const [savePassword, setSavePassword] = useState(true);
  const [busy, setBusy] = useState<"connect" | "save" | "delete" | null>(null);
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    appVersion().then(setVersion);
    connections
      .list()
      .then(setSaved)
      .catch((err) => setError(errorMessage(err, t)));
    // Only on open; a language change doesn't need the list again.
  }, []);

  function update<K extends keyof Draft>(key: K, value: Draft[K]) {
    setDraft((current) => ({ ...current, [key]: value }));
  }

  // A new connection checks the server's certificate unless the server is on
  // this machine, until the user picks an SSL mode.
  function changeHost(host: string) {
    setDraft((current) => ({
      ...current,
      host,
      sslMode: current.id || sslChosen ? current.sslMode : isLocalHost(host) ? "prefer" : "verify-full",
    }));
  }
  // Production starts read-only and takes no snapshot plans, unless set otherwise.
  const production = draft.environment === "production";
  const weakSsl =
    !isLocalHost(draft.host) && draft.sslMode !== "verify-full" ? t.connect.sslWeak[draft.sslMode] : null;

  function select(next: Draft) {
    setDraft(next);
    setSslChosen(false);
    setPassword("");
    setSavePassword(next.id === "" || next.hasPassword);
    setConfirmingDelete(false);
    setError(null);
  }

  async function connect(target: Draft, typedPassword: string) {
    setBusy("connect");
    setError(null);
    try {
      // For a saved connection, an empty password field means "use the saved
      // password". Its settings come along either way.
      const session = await pg.connect(
        {
          host: target.host,
          port: Number(target.port),
          user: target.user,
          database: target.database,
          sslMode: target.sslMode,
          password: typedPassword || null,
        },
        target.id || null,
      );
      onConnected(session, saved.find((connection) => connection.id === target.id) ?? null);
    } catch (err) {
      setError(errorMessage(err, t));
      setBusy(null);
    }
  }

  async function save() {
    setBusy("save");
    setError(null);
    try {
      const change: PasswordChange = !savePassword
        ? { kind: "forget" }
        : password
          ? { kind: "set", password }
          : { kind: "keep" };
      const result = await connections.save(
        { ...draft, port: Number(draft.port), name: draft.name.trim() || defaultName(draft) },
        change,
      );
      setSaved(await connections.list());
      select(toDraft(result));
    } catch (err) {
      setError(errorMessage(err, t));
    } finally {
      setBusy(null);
    }
  }

  async function remove() {
    if (!confirmingDelete) {
      setConfirmingDelete(true);
      return;
    }
    setBusy("delete");
    setError(null);
    try {
      await connections.delete(draft.id);
      setSaved(await connections.list());
      select(NEW_CONNECTION);
    } catch (err) {
      setError(errorMessage(err, t));
    } finally {
      setBusy(null);
    }
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    connect(draft, password);
  }

  return (
    <main className="connect">
      <div className="connect-card">
        <aside className="saved-list">
          <header>
            <h1>Dolmen</h1>
            {version && <span className="muted">v{version}</span>}
            <span className="spacer" />
            <SettingsButton onClick={() => setSettingsOpen(true)} />
          </header>
          <button
            className={`saved-item ${draft.id === "" ? "selected" : ""}`}
            onClick={() => select(NEW_CONNECTION)}
          >
            {t.connect.newConnection}
          </button>
          {saved.map((connection) => (
            <button
              key={connection.id}
              className={`saved-item ${connection.id === draft.id ? "selected" : ""}`}
              onClick={() => select(toDraft(connection))}
              onDoubleClick={() => connect(toDraft(connection), "")}
              title={t.connect.doubleClickToConnect}
            >
              <span className="saved-name">
                {connection.environment && <EnvironmentBadge environment={connection.environment} />}
                {connection.name}
              </span>
              <span className="muted">
                {connection.user}@{connection.host}:{connection.port}/{connection.database}
              </span>
            </button>
          ))}
        </aside>

        <form className="connect-form" onSubmit={submit}>
          <p className="muted">{t.connect.subtitle}</p>
          <div className="field-row">
            <label className="field grow">
              {t.connect.name}
              <input
                value={draft.name}
                placeholder={defaultName(draft)}
                onChange={(e) => update("name", e.target.value)}
              />
            </label>
            <label className="field" title={t.connect.environmentTitle}>
              {t.connect.environment}
              <select
                value={draft.environment ?? ""}
                onChange={(e) => update("environment", (e.target.value || null) as Environment | null)}
              >
                <option value="">{t.connect.noEnvironment}</option>
                {ENVIRONMENTS.map((environment) => (
                  <option key={environment} value={environment}>
                    {t.environments[environment]}
                  </option>
                ))}
              </select>
            </label>
          </div>
          <div className="field-row">
            <label className="field grow">
              {t.connect.host}
              <input value={draft.host} onChange={(e) => changeHost(e.target.value)} required />
            </label>
            <label className="field port">
              {t.connect.port}
              <input
                type="number"
                min={1}
                max={65535}
                value={draft.port}
                onChange={(e) => update("port", e.target.value)}
                required
              />
            </label>
          </div>
          <label className="field">
            {t.connect.database}
            <input
              value={draft.database}
              onChange={(e) => update("database", e.target.value)}
              required
            />
          </label>
          <div className="field-row">
            <label className="field grow">
              {t.connect.user}
              <input value={draft.user} onChange={(e) => update("user", e.target.value)} required />
            </label>
            <label className="field grow">
              {t.connect.password}
              <input
                type="password"
                value={password}
                placeholder={draft.hasPassword ? t.connect.savedInKeychain : ""}
                onChange={(e) => setPassword(e.target.value)}
              />
            </label>
          </div>
          <label className="field">
            {t.connect.sslMode}
            <select
              value={draft.sslMode}
              onChange={(e) => {
                setSslChosen(true);
                update("sslMode", e.target.value as SslMode);
              }}
            >
              {SSL_MODES.map((mode) => (
                <option key={mode} value={mode}>
                  {t.connect.sslModes[mode]}
                </option>
              ))}
            </select>
          </label>
          <label className="checkbox">
            <input
              type="checkbox"
              checked={savePassword}
              onChange={(e) => setSavePassword(e.target.checked)}
            />
            {t.connect.savePassword}
          </label>
          {weakSsl && <p className="error-text ssl-warning">{weakSsl}</p>}
          <label className="checkbox" title={t.connect.readOnlyTitle}>
            <input
              type="checkbox"
              checked={draft.readOnly ?? production}
              onChange={(e) => setDraft({ ...draft, readOnly: e.target.checked })}
            />
            {t.connect.readOnly}
          </label>
          <label className="checkbox" title={t.connect.allowSnapshotApplyTitle}>
            <input
              type="checkbox"
              checked={draft.allowSnapshotApply ?? (isLocalHost(draft.host) && !production)}
              onChange={(e) => setDraft({ ...draft, allowSnapshotApply: e.target.checked })}
            />
            {t.connect.allowSnapshotApply}
          </label>
          <label className="checkbox" title={t.connect.recordHistoryTitle}>
            <input
              type="checkbox"
              checked={draft.recordHistory}
              onChange={(e) => setDraft({ ...draft, recordHistory: e.target.checked })}
            />
            {t.connect.recordHistory}
          </label>

          {error && <p className="error-text">{error}</p>}
          <div className="actions">
            {draft.id && (
              <button type="button" className="danger" onClick={remove} disabled={busy !== null}>
                {confirmingDelete ? t.connect.confirmDelete : t.common.delete}
              </button>
            )}
            <span className="spacer" />
            <button type="button" onClick={save} disabled={busy !== null}>
              {busy === "save" ? t.common.saving : t.common.save}
            </button>
            <button type="submit" className="primary" disabled={busy !== null}>
              {busy === "connect" ? t.connect.connecting : t.connect.connect}
            </button>
          </div>
        </form>
      </div>
      {settingsOpen && <SettingsDialog onClose={() => setSettingsOpen(false)} />}
    </main>
  );
}

/** A short colored label saying what a database is for. */
export function EnvironmentBadge({ environment }: { environment: Environment }) {
  const t = useT();
  return <span className={`environment-badge ${environment}`}>{t.environments[environment]}</span>;
}
