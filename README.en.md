# Dolmen

**A database client that keeps your tables' history in Git**

[한국어](README.md) · **English**

<p align="center">
  <a href="https://github.com/whalebanana86/ai-dolmen/releases/latest"><b>⬇︎ Download (macOS · Windows · Linux)</b></a>
</p>

An open-source desktop app that connects to PostgreSQL, runs your SQL, lets you
change results right in the grid, and **records every change to your tables'
structure in Git**. It's an early version, and PostgreSQL is the only database
it connects to so far.

## What is it?

A database client sends SQL to a database and shows the results as a table:
think DataGrip, DBeaver or TablePlus. Dolmen does that, and adds three things.

- **Structure goes into Git.** Tables, views and functions are written out as
  files and committed to a repository next to your code, so who changed what,
  and when, is kept like code.
- **Go back like code.** Switch branches or pick an old commit, and your local
  database is brought to that structure.
- **Production stays safe.** Production databases open read-only, and statements
  that destroy data say how many rows they touch and ask first.

A *dolmen* — a "stone table" — is a monument that has stood for thousands of
years. About 40% of the world's dolmens are on the Korean peninsula, where
Dolmen is made. The name hopes your tables' history lasts as long.

## Why?

1. **"Who changed this column, and when?"** — Code is all in Git, but changes
   to the database's structure often aren't recorded anywhere.
2. **Every branch wants a different schema** — Change a table on a feature
   branch, go back to main, and your local database still has the change, so
   the app breaks.
3. **One mistake on production costs a lot** — an `UPDATE` without `WHERE`, or
   a production connection opened for the development one.
4. **Schema version control lives in expensive tools** — similar features are
   usually in the higher editions of commercial clients.

So Dolmen is a light, free, open-source client with schema version control
built in.

## Install

### Download

Get the file for your computer from the
[Releases page](https://github.com/whalebanana86/ai-dolmen/releases/latest).

| Computer | File |
|---|---|
| Mac (Apple silicon, M1 and later) | `Dolmen_…_aarch64.dmg` |
| Mac (Intel) | `Dolmen_…_x64.dmg` |
| Windows | `Dolmen_…_x64-setup.exe` or `Dolmen_…_x64_en-US.msi` |
| Linux | `.AppImage`, `.deb` or `.rpm` |

- **Mac**: open the dmg and drag Dolmen to Applications. If macOS says it can't
  check the developer, press **Open Anyway** at the bottom of **System Settings →
  Privacy & Security**; it's needed once, since the app isn't notarized yet. Or
  run `xattr -dr com.apple.quarantine /Applications/Dolmen.app` in Terminal.
- **Windows**: if SmartScreen stops it, choose **More info → Run anyway**.

### Updates

Press the gear at the bottom left, then **Updates → Check for updates**. Dolmen
compares itself with the latest release on GitHub; when there's a newer one,
press **Install and restart**. It installs only updates signed with the
project's key, and looks for one only when you ask. On Linux, a `.deb` or
`.rpm` install asks for your password to install it.

### Uninstall

Remove the app. To remove saved connections and the query history too, delete
the app's data folder (on a Mac, `~/Library/Application Support/dev.dolmendb.dolmen`).
Saved passwords are in the OS keychain under "Dolmen".

## Using it

### Connecting

On the first screen, **+ New connection** takes a host, port, database, user
and password; press **Connect**. Double-click a saved connection to open it.

- Passwords are kept only in the **OS keychain** (Keychain on macOS, Credential
  Manager on Windows, Secret Service on Linux), never in files or on screen.
- Give a connection an **environment** (development, staging, production) and
  the workspace shows a colored band. Production starts read-only.
- For a server on another computer, set the SSL mode to **Verify full**.

### The workspace

| Where | What it does |
|---|---|
| Toolbar | The connection's name (click for its address, version and **Disconnect**), the read-only lock, **Run** and **Plan** |
| Icons on the left | Schema tree, history, ERD and snapshot; the results panel and settings at the bottom. Hover for their names |
| Schema tree | Schemas, tables, views, functions, sequences and enums. Double-click to insert a query; right-click for details, export, import and creating objects |
| Editor | Where you write SQL. Each tab (⌘T) has its own connection, so transactions don't mix |
| Results | The result grid; running several statements gives a tab for each |

### Running queries

- **⌘Enter** runs only the statement at the cursor. With several selected, they
  run in turn and stop at the first failure (the ones before it aren't undone).
- Completion offers the connected database's real tables and columns.
- **Schema**, beside the tabs, picks where table names without a schema are
  found; tables elsewhere take their schema, as in `public.users`.
- Results show up to 10,000 rows and the total count. Select a cell and copy it
  with ⌘C; drag a column's edge to resize it, and double-click an edge to fit
  the columns to the grid again.
- Errors say the line and column in the editor.

**History** (⌘⇧H) lists the statements run on each connection, newest first,
to search, star, insert into the editor or run again. Edits and snapshots
applied from the app are listed too.

### Editing results

When a result comes from one table and includes its **primary key**, you can
change it in the grid.

- Double-click a cell or press Enter to edit it. Enter finishes, Shift+Enter
  starts a new line, Tab moves to the next column and Esc cancels.
- **Add row** adds one; cells left empty get the column's default.
- ⌘⌫ deletes a row (again to keep it), and the right-click menu sets NULL or
  undoes a change.
- Changed cells are yellow, new rows green and deleted ones red until you save.
  **Show SQL** shows the statements; **Save** (⌘S) runs them.

Saving runs in **one transaction**, so everything is saved or nothing is. Rows
are found by their primary key, and a row is changed only if the cells you
changed still hold the values you read: if someone changed or deleted it in the
meantime, nothing is saved and the row is pointed out. Inside a transaction you
opened with `BEGIN`, the changes join it and are saved when it commits. A
read-only connection can't be edited, and on production Dolmen shows the
statements and asks before saving. A result that can't be edited, such as a
join, says why when you double-click it.

### Schema snapshots: structure in Git

Open **Snapshot** on the left and choose where it goes: a folder in a Git
repository (such as `db/schema`), or a repository's address, which Dolmen
clones and pushes to on every commit.

Dolmen writes the database's structure as one file per object:

```
db/schema/
  public/tables/users.sql      CREATE TABLE …
  public/views/active_users.sql
  public/functions/…
  public/types/…
  dolmen-model.json            the structure, for going back
```

- **Changes** shows what changed since the last snapshot, file by file. Write a
  message and press **Commit** or **Commit & push**.
- **History** draws the repository's commit graph and compares any commit or
  tag with the database.
- Branches can be switched or created there too.

**It goes the other way, too.** Switch branches, pick a commit in History, or
check one out with Git, then press **Update database…**: Dolmen first shows the
statements that bring your *local* database to that structure. They run in one
transaction, so everything applies or nothing does. Steps that drop data or
change a column's type wait for your consent, and drops that look like renames
can be applied as renames to keep the data. Servers on other computers and
production databases can't be changed this way unless their connection allows it.

The same comparison runs from the command line and in CI; see
[the command-line tool](#the-command-line-tool).

### Details, relationships, ERD and new objects

- Right-click a table, view or function → **Show details**: columns, keys,
  constraints, indexes, DDL, and the tables it references and that reference it
  (with their ON DELETE and ON UPDATE rules).
- **ERD** draws a schema's tables and foreign keys, or one table with its
  neighbors.
- **Edit** in the details changes a table (name, columns, constraints, indexes,
  comments), a view's query or a function's definition; **New table…**, **New
  view…** and **New function…** make new ones. The statements are shown first
  and run in one transaction. Steps that can lose data need your consent, and
  the statements can go into the editor instead.

### Importing and exporting data

- **Export**: right-click a table or view → **Export data…**, or **Export…** on
  a result, to save CSV (with a byte order mark for Excel if you like), JSON or
  `INSERT` statements. A result's statement runs again read-only, so every row
  is saved, not only those shown.
- **Import**: right-click a table → **Import data…** to append a CSV file's rows
  in one transaction. Dolmen guesses the delimiter, matches the file's columns
  to the table's by name and lets you skip some. Choose EUC-KR (CP949) for files
  Korean Excel saves that way. A bad value stops the import with its line and
  column, and nothing is added.

### Production safety

- Set a connection's environment to **production**, and the workspace gets a red
  band and badge and starts **read-only**: every tab refuses to change data until
  you allow writing with the lock in the toolbar, which on production takes a
  second click.
- Before running an `UPDATE` or `DELETE` without `WHERE`, a `TRUNCATE`, a `DROP`
  or a dropped column, Dolmen names the database and about how many rows each
  statement touches, and asks.

### Query plans

**Plan** (⌘E) shows how PostgreSQL runs the statement at the cursor, and ⌘⇧E
measures it with `EXPLAIN ANALYZE`. Each step shows its rows (actual against
expected), the time it took and its share of the whole, with notes on the usual
causes of a slow query: a sequential scan that throws most rows away, an
estimate ten times off, a sort that spills to disk. Measuring runs on a separate
connection and is rolled back; a statement that changes data asks first.

### Settings

From the gear at the bottom left:

- **Language**: 한국어 / English. Korean to start with.
- **Theme**: follow the system / light / dark
- **Query history**: mask secrets, clear everything
- **Updates**: this version, and checking for a newer one

### Shortcuts

| Key | What it does |
|---|---|
| `⌘⏎` | Run the statement at the cursor (or the selected ones) |
| `⌘E` / `⌘⇧E` | Show the query plan / measure it |
| `⌘T` / `Ctrl+Tab` | New editor tab / next tab |
| `⌘B` / `⌘J` | Show or hide the schema tree / the results |
| `⌘⇧H` | History |
| `Enter` or `F2` | Edit a result's cell |
| `⌘⌫` | Delete a result's row (again to keep it) |
| `⌘S` | Save the changes to a result |
| `⌘C` | Copy a result's cell |

On Windows and Linux, use `Ctrl` for `⌘`.

## Databases

| Database | Status |
|---|---|
| PostgreSQL | Supported (v0.1) |
| MySQL, MariaDB | Planned (v0.2) |
| Redis, Valkey | Planned (v0.3) |

## Is my data safe?

- **No sign-up**, and no usage statistics. The app connects only to the
  databases you connect, to GitHub when you check for updates, and to a
  snapshot's Git repository when you use one by address. The window's web view
  can't reach the network at all.
- **Passwords** are kept only in the OS keychain.
- **Query history** stays on this computer in a SQLite file only you can read.
  It keeps 90 days and up to 10,000 queries per connection (starred ones until
  you remove them), masks passwords and connection secrets, and never keeps
  result rows. Turn it off per connection for databases whose SQL shouldn't be kept.
- Changes to data are always shown first and run in one transaction. Production
  starts read-only, and snapshots change only databases on this computer unless
  a connection allows more.

## What's next

- MySQL and MariaDB (v0.2), Redis and Valkey (v0.3)
- **AI that understands your data**: natural language to SQL that knows your
  schema and its comments, explanations and error fixes. It's built, but left
  out of releases for now ([try it in a development build](#ai-development-builds))
- Performance diagnostics and index suggestions
- Apple notarization and Windows signing

---

## Details for developers

### How it's built

| | |
|---|---|
| App | Tauri 2 (Rust) + React 19 + TypeScript, with CodeMirror 6 for the editor |
| Database | tokio-postgres. Each editor tab has its own connection; reading the structure and applying schema changes use another (`search_path=pg_catalog`). Values are shown in PostgreSQL's text format, so bigint and numeric keep their precision |
| Editing results | The statement is prepared again to learn each column's table and column number. Saving finds rows by primary key and locks optimistically, comparing the changed cells with the values read, as `format('%s', …)` writes them. Inside an open transaction it joins as a savepoint |
| Schema snapshots | One DDL file per object, plus `dolmen-model.json`. The plan to apply one comes from comparing models, and runs in one transaction |
| Storage | Saved connections in a JSON file in the app's config folder, passwords in the OS keychain (keyring), history and editor tabs in SQLite |
| Updates | tauri-plugin-updater; the minisign signature and the version it was signed for (`requireSignedVersion`) are checked before installing |

### The command-line tool

The same snapshot writing and comparing, from the command line: for example in
CI, to fail when production drifts from the repository.

```sh
cargo install --git https://github.com/whalebanana86/ai-dolmen dolmen-cli

dolmen schema dump  --url postgres://user@host/db --dir db/schema   # write the snapshot
dolmen schema check --url postgres://user@host/db --dir db/schema   # compare
```

`check` exits with 0 when the database matches, 1 when it differs (drift) and 2
on errors. `--url` falls back to `DATABASE_URL`, and the password to `PGPASSWORD`.

```yaml
# .github/workflows/schema.yml
- name: Fail when production drifts from db/schema
  run: dolmen schema check --dir db/schema
  env:
    DATABASE_URL: ${{ secrets.DATABASE_URL }}
```

### Development

Prerequisites: Rust (stable), Node.js 20.19+ or 22.12+, pnpm 9, and the
[Tauri system dependencies](https://tauri.app/start/prerequisites/) for your platform.

```sh
cd app
pnpm install
pnpm tauri dev                     # development build
pnpm build                         # type check and build the UI
cargo run -p dolmen-cli -- --help  # the command-line tool
```

A PostgreSQL for development and tests (Docker required):

```sh
docker compose up -d   # PostgreSQL 18 on localhost:55432, user and password "postgres"
DOLMEN_TEST_PG_HOST=localhost DOLMEN_TEST_PG_PORT=55432 cargo test
```

Without `DOLMEN_TEST_PG_HOST`, tests that need a server are skipped.

### Releasing

Set the same version in `Cargo.toml` (`[workspace.package]`) and
`app/package.json`, commit, and push a matching tag such as `v0.2.0`. The
[Release workflow](.github/workflows/release.yml) builds the installers for
macOS (Apple silicon and Intel), Windows and Linux, attaches them to a release,
checks that `latest.json` has an update for all four platforms, and publishes
it. It signs and notarizes the macOS app when the Apple secrets it lists are set.

Update packages are signed with the private key in the `TAURI_SIGNING_PRIVATE_KEY`
secret, and installed apps accept only updates signed with it; its public key
is in `app/src-tauri/tauri.conf.json`. Keep the private key safe and never
replace it, or installed apps can't update any more. To build a release
locally, set `TAURI_SIGNING_PRIVATE_KEY` to the key or its path and run
`pnpm tauri build` in `app`.

### AI (development builds)

AI is left out of releases for now. Turn it on with `VITE_DOLMEN_AI=1 pnpm tauri dev`
in `app`.

Press **AI** (⌘K) in the editor to ask Claude for SQL in plain language, or to
explain the selected SQL. When a statement fails, **Fix with AI** proposes a
corrected one. Requests carry the database's structure as the snapshot's DDL,
with its comments. Bring your own [Anthropic API key](https://platform.claude.com/):
add it in Settings, where it goes to the OS keychain, or set `ANTHROPIC_API_KEY`.
Claude Opus 5.5 is the default model. Dolmen never runs SQL from AI on its own
and doesn't read table rows for AI, but values in the SQL you ask about and in
error messages are sent along with the schema.

### Repository layout

```
crates/dolmen-core   Shared core: connections, reading structure, schema versioning, editing results
crates/dolmen-cli    The dolmen command-line tool
app/                 Desktop app UI (Tauri 2 + React)
app/src-tauri        Desktop app backend (Rust)
```

## License

[MIT](LICENSE). Anyone may use, change and share it for free, commercially too,
as long as the copyright notice stays.
