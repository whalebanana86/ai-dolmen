import { FormEvent, useEffect, useState } from "react";
import {
  PasswordChange,
  SavedConnection,
  SessionInfo,
  SslMode,
  appVersion,
  connections,
  errorMessage,
  pg,
} from "./api";

const SSL_MODES: { value: SslMode; label: string }[] = [
  { value: "disable", label: "Disable" },
  { value: "prefer", label: "Prefer" },
  { value: "require", label: "Require" },
  { value: "verify-full", label: "Verify full" },
];

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
  const [version, setVersion] = useState("");
  const [saved, setSaved] = useState<SavedConnection[]>([]);
  const [draft, setDraft] = useState<Draft>(NEW_CONNECTION);
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
      .catch((err) => setError(errorMessage(err)));
  }, []);

  function update<K extends keyof Draft>(key: K, value: Draft[K]) {
    setDraft((current) => ({ ...current, [key]: value }));
  }

  function select(next: Draft) {
    setDraft(next);
    setPassword("");
    setSavePassword(next.id === "" || next.hasPassword);
    setConfirmingDelete(false);
    setError(null);
  }

  async function connect(target: Draft, typedPassword: string) {
    setBusy("connect");
    setError(null);
    try {
      // For a saved connection, an empty password field means "use the saved password".
      const useSavedPassword = target.hasPassword && typedPassword === "";
      const session = await pg.connect(
        {
          host: target.host,
          port: Number(target.port),
          user: target.user,
          database: target.database,
          sslMode: target.sslMode,
          password: typedPassword || null,
        },
        useSavedPassword ? target.id : null,
      );
      onConnected(session, saved.find((connection) => connection.id === target.id) ?? null);
    } catch (err) {
      setError(errorMessage(err));
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
      setError(errorMessage(err));
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
      setError(errorMessage(err));
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
          </header>
          <button
            className={`saved-item ${draft.id === "" ? "selected" : ""}`}
            onClick={() => select(NEW_CONNECTION)}
          >
            + New connection
          </button>
          {saved.map((connection) => (
            <button
              key={connection.id}
              className={`saved-item ${connection.id === draft.id ? "selected" : ""}`}
              onClick={() => select(toDraft(connection))}
              onDoubleClick={() => connect(toDraft(connection), "")}
              title="Double-click to connect"
            >
              <span>{connection.name}</span>
              <span className="muted">
                {connection.user}@{connection.host}:{connection.port}/{connection.database}
              </span>
            </button>
          ))}
        </aside>

        <form className="connect-form" onSubmit={submit}>
          <p className="muted">PostgreSQL</p>
          <label className="field">
            Name
            <input
              value={draft.name}
              placeholder={defaultName(draft)}
              onChange={(e) => update("name", e.target.value)}
            />
          </label>
          <div className="field-row">
            <label className="field grow">
              Host
              <input value={draft.host} onChange={(e) => update("host", e.target.value)} required />
            </label>
            <label className="field port">
              Port
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
            Database
            <input
              value={draft.database}
              onChange={(e) => update("database", e.target.value)}
              required
            />
          </label>
          <div className="field-row">
            <label className="field grow">
              User
              <input value={draft.user} onChange={(e) => update("user", e.target.value)} required />
            </label>
            <label className="field grow">
              Password
              <input
                type="password"
                value={password}
                placeholder={draft.hasPassword ? "Saved in keychain" : ""}
                onChange={(e) => setPassword(e.target.value)}
              />
            </label>
          </div>
          <label className="field">
            SSL mode
            <select value={draft.sslMode} onChange={(e) => update("sslMode", e.target.value as SslMode)}>
              {SSL_MODES.map((mode) => (
                <option key={mode.value} value={mode.value}>
                  {mode.label}
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
            Save password in the OS keychain
          </label>

          {error && <p className="error-text">{error}</p>}
          <div className="actions">
            {draft.id && (
              <button type="button" className="danger" onClick={remove} disabled={busy !== null}>
                {confirmingDelete ? "Click again to delete" : "Delete"}
              </button>
            )}
            <span className="spacer" />
            <button type="button" onClick={save} disabled={busy !== null}>
              {busy === "save" ? "Saving…" : "Save"}
            </button>
            <button type="submit" className="primary" disabled={busy !== null}>
              {busy === "connect" ? "Connecting…" : "Connect"}
            </button>
          </div>
        </form>
      </div>
    </main>
  );
}
