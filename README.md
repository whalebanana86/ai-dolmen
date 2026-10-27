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

## Schema snapshots

Dolmen writes a database's structure into a folder as one DDL file per object
(`public/tables/users.sql`, `public/views/…`, `functions/`, `types/`), so it can
live in Git next to your code. In the app, **Snapshot** shows what changed since
the last snapshot, writes it, and commits and pushes the folder; **History**
draws the repository's graph and compares any commit or tag with the database.

Snapshots can also go the other way, like checking out code: switch branches
in the snapshot panel, pick a commit or tag in **History**, or check one out
with Git and open **Changes**, then **Update database** shows the DDL plan that
makes a *local* database match it.
The plan runs in one transaction, so it applies completely or not at all. Steps
that drop data or change column types stay blocked until you allow them, and
drops that look like renames can be applied as renames to keep the data. Each
snapshot keeps the schema model it came from in `dolmen-model.json` for this.

The same comparison runs from the command line, for example in CI:

```sh
cargo install --git https://github.com/whalebanana86/ai-dolmen dolmen-cli

dolmen schema dump  --url postgres://user@host/db --dir db/schema   # write the snapshot
dolmen schema check --url postgres://user@host/db --dir db/schema   # compare
```

`check` exits with 0 when the database matches, 1 when it differs (drift) and 2
on errors. `--url` falls back to `DATABASE_URL`, and the password to `PGPASSWORD`:

```yaml
# .github/workflows/schema.yml
- name: Fail when production drifts from db/schema
  run: dolmen schema check --dir db/schema
  env:
    DATABASE_URL: ${{ secrets.DATABASE_URL }}
```

## Details, relationships and ERD

Right-click a table, view or function and choose **Show details** for its
columns, keys, constraints, indexes and DDL. A table's details list the tables
it references and the ones that reference it, with their ON DELETE and ON
UPDATE rules. **ERD** draws a schema's tables and foreign keys as a diagram,
or one table with its neighbors.

**Edit** in the details changes a table (name, columns, constraints, indexes,
comments), a view's query or a function's definition. Dolmen shows the
statements first; they run in one transaction, steps that can lose data need
your consent, and they can go into the editor instead.

## AI

Press **AI** (⌘K) in the editor to ask Claude to write SQL for a request in
plain language, or to explain the selected SQL. When a statement fails, **Fix
with AI** proposes a corrected one that can replace it. Requests carry the
database's structure as the schema snapshot's DDL, with its comments, so
answers use your real tables and the meaning you wrote down for them.

Bring your own [Anthropic API key](https://platform.claude.com/): add it in
Settings (⚙), where it goes to the OS keychain, or set `ANTHROPIC_API_KEY`.
Claude Opus 5.5 is the default model; Sonnet 5.5 and Haiku 5.5 are faster and
cheaper. Dolmen never runs SQL from AI on its own, and it doesn't read table
rows for AI, but values in the SQL you ask about and in error messages are sent
along with the schema.

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
