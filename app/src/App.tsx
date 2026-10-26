import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

function App() {
  const [version, setVersion] = useState("");

  useEffect(() => {
    invoke<string>("app_version").then(setVersion);
  }, []);

  return (
    <main className="container">
      <h1>Dolmen</h1>
      {version && <p className="version">v{version}</p>}
    </main>
  );
}

export default App;
