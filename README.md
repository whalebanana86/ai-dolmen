# Dolmen

English | [한국어](README.ko.md)

An open-source, AI-native database client with schema version control built in.

> **Status:** early preview. Builds for macOS, Windows and Linux are published on the [Releases](https://github.com/whalebanana86/ai-dolmen/releases) page.

A *dolmen* — a "stone table" — is a megalithic monument that has stood for thousands of years. The Korean peninsula, where Dolmen is made, is home to roughly 40% of the world's dolmens. Dolmen aims to make the history of your tables just as durable: every schema change is recorded in Git.

## Install

Download the installer for your platform from
[Releases](https://github.com/whalebanana86/ai-dolmen/releases): a `.dmg` for
macOS (`aarch64` for Apple silicon, `x64` for Intel), a `-setup.exe` or `.msi`
for Windows, or an `.AppImage`, `.deb` or `.rpm` for Linux.

The builds aren't signed by a known developer yet. macOS says it can't check
the app: open it from *System Settings → Privacy & Security → Open Anyway*, or
run `xattr -dr com.apple.quarantine /Applications/Dolmen.app` once. On Windows,
SmartScreen asks; choose *More info → Run anyway*.

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

Right-click a schema, a group or an object for **New table…**, **New view…** or
**New function…**, written and reviewed the same way. **Edit** in the details changes a table (name, columns, constraints, indexes,
comments), a view's query or a function's definition. Dolmen shows the
statements first; they run in one transaction, steps that can lose data need
your consent, and they can go into the editor instead.

## Query history

The editor has tabs (⌘T), each a console with its own session, so transactions,
the search path and cancelling stay apart; the schema picker beside the tabs
shows and sets the one schema the active tab's unqualified names resolve in;
other schemas' tables take their schema, as in `public.users`.

**History** (⌘⇧H) lists the statements run on each connection, newest first,
to search, star, insert into the editor or run again; edits and snapshots
applied from the app are listed too. It stays on this computer in a SQLite file
readable only by you, keeps 90 days and up to 10,000 queries per connection
(starred ones until you remove them), masks passwords and connection secrets,
and never keeps result rows. Turn it off per connection for databases whose SQL
shouldn't be kept. The editor's text is kept per connection as well.

## Data import and export

Right-click a table or view and choose **Export data…**, or press **Export…**
on a result, to save every row as CSV (with a byte order mark for Excel if you
like), JSON or `INSERT` statements. A result's statement runs again in a
read-only transaction, so all its rows are saved, not only the ones shown.

**Import data…** on a table appends a CSV file's rows with `COPY`, in one
transaction. Files are read as UTF-8, and one that isn't is refused rather
than imported garbled; choose EUC-KR (CP949) for files Korean Excel saves that
way. Dolmen guesses the delimiter, matches the file's columns to the table's by
name and lets you skip some. A bad value stops the import with its line and column, and
nothing is added.

## Production safety

Give a saved connection an environment (development, staging or production)
and the workspace shows it as a colored band and badge. Production connections
start **read-only**: every tab's session refuses to change data until you allow
writing with the lock in the toolbar, which on production takes a second click.
While locked, edits from the detail panel and snapshot updates are refused too.
Both defaults can be changed per connection.

Before running an `UPDATE` or `DELETE` without `WHERE`, a `TRUNCATE`, a `DROP`
or a dropped column, the editor asks first, naming the database and about how
many rows each statement touches.

## Query plans

**Plan** (⌘E) shows how PostgreSQL runs the statement at the cursor, and ⌘⇧E
measures it with `EXPLAIN ANALYZE`. Each step shows its rows (actual against
expected), the time it took itself and its share of the whole, with notes on
the usual causes of a slow query: a sequential scan that throws most rows away,
an estimate ten times off, a sort that spills to disk. Measuring runs the
statement on a separate session inside a transaction that is rolled back; for
a statement that changes data, the plan asks before measuring.

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

To release, set the version in `Cargo.toml` (`[workspace.package]`) and
`app/package.json`, commit, and push a matching tag such as `v0.2.0`. The
[Release workflow](.github/workflows/release.yml) builds the installers and
attaches them to a draft release to review and publish. It signs and notarizes
the macOS app when the Apple secrets it lists are set.

## Repository layout

```
crates/dolmen-core   Shared core: connections, introspection, schema versioning
crates/dolmen-cli    The `dolmen` command-line tool
app/                 Desktop app frontend (Tauri 2 + React)
app/src-tauri        Desktop app backend (Rust)
```

## License

[Apache License 2.0](LICENSE)
