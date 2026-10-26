import { useState } from "react";
import { SessionInfo } from "./api";
import { ConnectionForm } from "./ConnectionForm";
import { QueryPanel } from "./QueryPanel";
import "./App.css";

function App() {
  const [session, setSession] = useState<SessionInfo | null>(null);

  return session ? (
    <QueryPanel session={session} onDisconnect={() => setSession(null)} />
  ) : (
    <ConnectionForm onConnected={setSession} />
  );
}

export default App;
