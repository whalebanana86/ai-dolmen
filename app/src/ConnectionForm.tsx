import { FormEvent, useEffect, useState } from "react";
import { SessionInfo, SslMode, appVersion, errorMessage, pg } from "./api";

const SSL_MODES: { value: SslMode; label: string }[] = [
  { value: "disable", label: "Disable" },
  { value: "prefer", label: "Prefer" },
  { value: "require", label: "Require" },
  { value: "verify-full", label: "Verify full" },
];

interface Props {
  onConnected: (session: SessionInfo) => void;
}

export function ConnectionForm({ onConnected }: Props) {
  const [version, setVersion] = useState("");
  const [host, setHost] = useState("localhost");
  const [port, setPort] = useState("5432");
  const [database, setDatabase] = useState("postgres");
  const [user, setUser] = useState("postgres");
  const [password, setPassword] = useState("");
  const [sslMode, setSslMode] = useState<SslMode>("prefer");
  const [connecting, setConnecting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    appVersion().then(setVersion);
  }, []);

  async function connect(event: FormEvent) {
    event.preventDefault();
    setConnecting(true);
    setError(null);
    try {
      const session = await pg.connect({
        host,
        port: Number(port),
        user,
        password: password || null,
        database,
        sslMode,
      });
      onConnected(session);
    } catch (err) {
      setError(errorMessage(err));
      setConnecting(false);
    }
  }

  return (
    <main className="connect">
      <form className="connect-card" onSubmit={connect}>
        <header>
          <h1>Dolmen</h1>
          {version && <span className="muted">v{version}</span>}
        </header>
        <p className="muted">Connect to PostgreSQL</p>

        <div className="field-row">
          <label className="field grow">
            Host
            <input value={host} onChange={(e) => setHost(e.target.value)} required />
          </label>
          <label className="field port">
            Port
            <input
              type="number"
              min={1}
              max={65535}
              value={port}
              onChange={(e) => setPort(e.target.value)}
              required
            />
          </label>
        </div>
        <label className="field">
          Database
          <input value={database} onChange={(e) => setDatabase(e.target.value)} required />
        </label>
        <div className="field-row">
          <label className="field grow">
            User
            <input value={user} onChange={(e) => setUser(e.target.value)} required />
          </label>
          <label className="field grow">
            Password
            <input type="password" value={password} onChange={(e) => setPassword(e.target.value)} />
          </label>
        </div>
        <label className="field">
          SSL mode
          <select value={sslMode} onChange={(e) => setSslMode(e.target.value as SslMode)}>
            {SSL_MODES.map((mode) => (
              <option key={mode.value} value={mode.value}>
                {mode.label}
              </option>
            ))}
          </select>
        </label>

        {error && <p className="error-text">{error}</p>}
        <button type="submit" className="primary" disabled={connecting}>
          {connecting ? "Connecting…" : "Connect"}
        </button>
      </form>
    </main>
  );
}
