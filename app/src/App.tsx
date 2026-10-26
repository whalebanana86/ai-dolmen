import { useState } from "react";
import { SavedConnection, SessionInfo } from "./api";
import { ConnectionForm } from "./ConnectionForm";
import { QueryPanel } from "./QueryPanel";
import "./App.css";

function App() {
  const [session, setSession] = useState<SessionInfo | null>(null);
  // The saved connection the session came from, if any; it holds the snapshot folder.
  const [connection, setConnection] = useState<SavedConnection | null>(null);

  return session ? (
    <QueryPanel
      session={session}
      connection={connection}
      onConnectionChange={setConnection}
      onDisconnect={() => {
        setSession(null);
        setConnection(null);
      }}
    />
  ) : (
    <ConnectionForm
      onConnected={(newSession, savedConnection) => {
        setSession(newSession);
        setConnection(savedConnection);
      }}
    />
  );
}

export default App;
