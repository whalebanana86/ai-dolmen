# Dolmen

English | [한국어](README.ko.md)

An open-source, AI-native database client with schema version control built in.

> **Status:** early development. There is nothing to download yet.

A *dolmen* — a "stone table" — is a megalithic monument that has stood for thousands of years. The Korean peninsula, where Dolmen is made, is home to roughly 40% of the world's dolmens. Dolmen aims to make the history of your tables just as durable: every schema change is recorded in Git.

## Planned features

- **Schema version control in Git** — changes made in the GUI or the SQL editor are captured as a readable schema snapshot and migration files, committed to your repository. Drift detection tells you when a database no longer matches the repository.
- **AI that understands your data** — schema-aware natural language to SQL, query explanations and error fixes, using your own API key. Context comes from your schema, its Git history and your codebase.
- **Safe by default** — AI-generated queries run read-only, changes show their impact before they run, and production connections are protected.
- **Depth** — query plan visualization and performance diagnostics.

## Databases

| Database | Status |
| --- | --- |
| PostgreSQL | First target (v0.1) |
| MySQL, MariaDB | Planned (v0.2) |
| Redis, Valkey | Planned (v0.3) |

## Development

Prerequisites:

- Rust (stable)
- Node.js 20.19+ or 22.12+
- pnpm 9
- [Tauri system dependencies](https://tauri.app/start/prerequisites/) for your platform

```sh
# Desktop app
cd app
pnpm install
pnpm tauri dev

# CLI
cargo run -p dolmen-cli -- --help
```

A local PostgreSQL for development and tests (Docker required):

```sh
docker compose up -d   # PostgreSQL 18 on localhost:55432, user and password "postgres"
DOLMEN_TEST_PG_HOST=localhost DOLMEN_TEST_PG_PORT=55432 cargo test
```

Without `DOLMEN_TEST_PG_HOST`, tests that need a server are skipped.

## Repository layout

```
crates/dolmen-core   Shared core: connections, introspection, schema versioning
crates/dolmen-cli    The `dolmen` command-line tool
app/                 Desktop app frontend (Tauri 2 + React)
app/src-tauri        Desktop app backend (Rust)
```

## License

[Apache License 2.0](LICENSE)
