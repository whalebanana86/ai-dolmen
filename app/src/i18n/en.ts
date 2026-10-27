// English messages. Every other language must provide the same keys: its
// catalog is typed as `Messages`, so a missing or extra key fails the build.

const plural = (n: number, one: string, many: string) => `${n.toLocaleString()} ${n === 1 ? one : many}`;

export const en = {
  common: {
    cancel: "Cancel",
    close: "Close",
    save: "Save",
    saving: "Saving…",
    delete: "Delete",
    change: "Change…",
    loading: "Loading…",
    settings: "Settings",
  },

  errors: {
    closed: "The connection is closed.",
    refused: {
      notLocal:
        "Only local databases (localhost, a loopback address or a Unix socket) can be changed from a snapshot.",
      planChanged: "The database or the snapshot changed since you reviewed the plan. Review it again.",
      planBlocked: "The plan has changes Dolmen can't make. See the plan.",
      destructiveNotAllowed:
        "The plan drops data or changes column types. Allow that explicitly to apply it.",
      noModel:
        "This snapshot has no dolmen-model.json, so it can't be applied: it was written before Dolmen saved models. Write and commit a new snapshot to apply it later.",
      badSnapshotPath: "The snapshot folder must be a path inside the repository, such as db/schema.",
      uncommittedChanges:
        "The snapshot folder has changes that aren't committed. Commit or discard them before switching branches.",
    },
    ai: {
      noApiKey: "There's no Anthropic API key yet. Add one in Settings (⚙) to use AI.",
      badApiKey: (message: string) =>
        `The Anthropic API didn't accept the API key (${message}). Check it in Settings (⚙).`,
      rateLimited: "Too many requests for this API key's limits. Try again in a moment.",
      unavailable: "The Anthropic API is busy or having trouble. Try again in a moment.",
      declined: "The Anthropic API's safety checks declined this request.",
      cancelled: "Stopped.",
      network: (message: string) => `Couldn't reach the Anthropic API: ${message}`,
    },
  },

  settings: {
    title: "Settings",
    language: "Language",
    theme: "Theme",
    themes: { system: "Match the system", light: "Light", dark: "Dark" },
    ai: {
      title: "AI",
      model: "Model",
      models: {
        "claude-opus-5-5": "Claude Opus 5.5: most accurate (default)",
        "claude-sonnet-5-5": "Claude Sonnet 5.5: faster and cheaper",
        "claude-haiku-5-5": "Claude Haiku 5.5: fastest and cheapest",
      },
      apiKey: "Anthropic API key",
      keySaved: "Saved in the OS keychain.",
      keyFromEnvironment: "Using the key in ANTHROPIC_API_KEY. A saved key takes its place.",
      keyPlaceholder: "sk-ant-…",
      saveKey: "Save key",
      forgetKey: "Remove key",
      keyHelp:
        "Use your own key from the Anthropic Console; Anthropic bills it for what you use. Dolmen keeps it in the OS keychain and sends it only to the Anthropic API.",
      privacy:
        "AI requests send the database's structure (tables, columns, comments), the SQL you ask about and its error messages to Anthropic. Dolmen doesn't read table rows for AI, but values written in the SQL or in an error message go along.",
    },
  },

  connect: {
    subtitle: "PostgreSQL",
    newConnection: "+ New connection",
    doubleClickToConnect: "Double-click to connect",
    name: "Name",
    host: "Host",
    port: "Port",
    database: "Database",
    user: "User",
    password: "Password",
    savedInKeychain: "Saved in keychain",
    sslMode: "SSL mode",
    sslModes: { disable: "Disable", prefer: "Prefer", require: "Require", "verify-full": "Verify full" },
    savePassword: "Save password in the OS keychain",
    confirmDelete: "Click again to delete",
    connect: "Connect",
    connecting: "Connecting…",
  },

  workspace: {
    run: "Run",
    runTitle: "Runs the statement at the cursor, or the selection (⌘/Ctrl + Enter)",
    disconnect: "Disconnect",
    snapshot: "Snapshot",
    snapshotTitle: "Compare the database with its schema snapshot",
    erd: "ERD",
    erdTitle: "Show the tables and their relationships as a diagram",
    running: "Running…",
    runStatus: (line: number, done: number, total: number, ms: number) =>
      `Line ${line} · ${done} of ${plural(total, "statement", "statements")} · ${ms} ms`,
    errorLocation: (line: number, column: number) => `line ${line}, column ${column}`,
    hint: "Hint:",
    locationNotSaved: (error: string) =>
      `The location is in use but couldn't be saved with the connection: ${error}`,
    toggleTree: (shown: boolean) => `${shown ? "Hide" : "Show"} the schema tree (⌘B)`,
    toggleResults: (shown: boolean) => `${shown ? "Hide" : "Show"} the results (⌘J)`,
    resizeTree: "Drag to resize the schema tree; double-click to reset",
    resizeEditor: "Drag to resize the editor and results; double-click to reset",
    resizePanel: "Drag to resize the panel; double-click to reset",
  },

  results: {
    statement: (n: number) => `Statement ${n}`,
    rowsAffected: (n: number) => plural(n, "row affected", "rows affected"),
    completed: "completed",
    rows: (n: number) => plural(n, "row", "rows"),
    firstRows: (shown: number, total: number) =>
      `showing first ${shown.toLocaleString()} of ${total.toLocaleString()} rows`,
  },

  tree: {
    filter: "Filter",
    reload: "Reload the database structure",
    notLoaded: "Not loaded",
    groups: {
      tables: "Tables",
      views: "Views",
      functions: "Functions",
      sequences: "Sequences",
      enums: "Enums",
    },
    doubleClickToQuery: "Double-click to query",
    menu: {
      refresh: "Refresh",
      dropSchema: "Drop schema…",
      showErd: "Show ERD",
      showDetail: "Show details",
      showInErd: "Show in ERD",
      selectRows: "Select rows",
      countRows: "Count rows",
      generateSelect: "Generate SELECT",
      generateInsert: "Generate INSERT",
      generateRefresh: "Generate REFRESH",
      showDdl: "Show DDL",
      copyName: "Copy name",
      copyQualifiedName: "Copy qualified name",
      truncate: "Truncate…",
      drop: "Drop…",
      showValueCounts: "Show value counts",
      copySignature: "Copy signature",
      copyDefinition: "Copy definition",
      showCurrentValue: "Show current value",
    },
  },

  ai: {
    button: "AI",
    buttonTitle: "Ask AI (⌘K)",
    placeholder: "What do you need? For example: sign-ups per day last month",
    generate: "Write SQL",
    explain: "Explain SQL",
    explainTitle: "Explain the selected SQL, or the statement at the cursor",
    fix: "Fix with AI",
    thinking: "Thinking…",
    stop: "Stop",
    insert: "Insert into editor",
    replace: "Replace the failed statement",
    asked: {
      generate: (request: string) => `“${request}”`,
      explain: "Explaining the SQL",
      fix: "Fixing the failed statement",
    },
    cutOff: "The answer was cut off because it got too long.",
    neverRuns: "Dolmen never runs SQL from AI on its own. Review it, then run it yourself.",
    openSettings: "Open settings",
  },

  detail: {
    kinds: { table: "Table", view: "View", materializedView: "Materialized view", function: "Function", procedure: "Procedure" },
    tabs: { detail: "Details", ddl: "DDL" },
    edit: "Edit",
    erd: "ERD",
    notFound: "This object is gone. It may have been renamed or dropped.",
    columns: "Columns",
    column: "Column",
    type: "Type",
    nullable: "Null",
    notNull: "NOT NULL",
    default: "Default",
    comment: "Comment",
    noComment: "No comment",
    constraints: "Constraints",
    indexes: "Indexes",
    partitionKey: "Partitioned by",
    definition: "Definition",
    arguments: "Arguments",
    none: "None",
    relationships: "Relationships",
    references: "References",
    referencedBy: "Referenced by",
    noRelationships: "No foreign keys to or from this table.",
    onDelete: "ON DELETE",
    onUpdate: "ON UPDATE",
    primaryKey: "Primary key",
    foreignKeyTo: (table: string) => `Foreign key to ${table}`,
    identity: (kind: string) => `Identity (${kind})`,
    generated: (expression: string) => `Generated: ${expression}`,
    copy: "Copy",
    copied: "Copied",
    insert: "Insert into editor",
    editing: {
      name: "Name",
      addColumn: "+ Column",
      removeColumn: "Remove column",
      restoreColumn: "Keep column",
      newColumn: "new_column",
      addConstraint: "+ Constraint",
      addIndex: "+ Index",
      remove: "Remove",
      restore: "Keep",
      constraintKinds: {
        primaryKey: "Primary key",
        unique: "Unique",
        check: "Check",
        foreignKey: "Foreign key",
      },
      constraintName: "Constraint name",
      constraintColumns: "Columns (comma-separated)",
      checkExpression: "Condition, e.g. price >= 0",
      referencedTable: "Referenced table",
      referencedColumns: "Referenced columns",
      indexName: "Index name",
      indexColumns: "Columns or expressions (comma-separated)",
      unique: "Unique",
      add: "Add",
      viewQuery: "Query",
      functionDefinition: "CREATE OR REPLACE statement",
      review: "Review changes…",
      cancel: "Cancel",
      reviewTitle: "Statements to run",
      noChanges: "Nothing has changed.",
      back: "Back to editing",
      apply: "Apply",
      applying: "Applying…",
      applied: "Applied.",
      allowDestructive: (n: number) => `Allow ${plural(n, "step", "steps")} that can lose data`,
      losesData: "Can lose data",
      oneTransaction: "The statements run in one transaction: all of them, or none if one fails.",
      emptyName: "The table and every column need a name, and every column a type.",
      duplicateColumn: (name: string) => `Two columns are named ${name}.`,
    },
  },

  erd: {
    title: "ERD",
    schema: "Schema",
    allSchemas: "All schemas",
    onlyRelated: (table: string) => `Only ${table} and its neighbors`,
    empty: "This schema has no tables.",
    help: "Drag to move tables, scroll to zoom. Double-click a table for its details.",
  },

  snapshot: {
    title: "Schema snapshot",
    tabs: { changes: "Changes", history: "History" },
    compareAgain: "Compare again",
    pullAndCompare: "Pull new commits and compare again",
    chooseFolderTitle: "Choose the schema snapshot folder",
    gettingRepository: "Getting the repository…",
    cloneLocation: (path: string) => `Dolmen keeps its copy in ${path} and pushes each commit.`,
    notInRepository: "Not inside a Git repository: the files can be written but not committed.",
    behind: (n: number) => `${n} behind`,
    publishBranch: "Publish branch",
    push: (n: number) => `Push ${n}`,
    pushed: "Pushed",
    firstWrite: "The first write sets this folder up as a snapshot folder.",
    updatedDatabase: (label: string) => `Updated the database to match ${label}`,
    latestSnapshot: "the latest snapshot",
    snapshotFiles: "the snapshot files",
    httpsHint:
      "Pushing over HTTPS uses the GitHub account your keychain has saved. If this Mac has more than one account, an SSH address picks the account by its key:",
    sshHint:
      "SSH to this host uses your default key, which may belong to another GitHub account. An alias in ~/.ssh/config picks the account by its key:",
    useAddress: (url: string) => `Use ${url}`,

    branches: {
      branch: "Branch",
      detached: "(no branch)",
      newBranch: "New branch",
      namePlaceholder: "feature/widen-names",
      create: "Create",
      created: (name: string) => `Created branch ${name}`,
      switched: (name: string) => `Switched to ${name}`,
      label: (name: string) => `branch ${name}`,
      unsaved: (n: number, name: string) =>
        `The database has ${plural(n, "change", "changes")} not in the snapshot. Switching to ${name} and updating the database to match it would undo them.`,
      switchAnyway: "Switch anyway",
    },

    location: {
      intro:
        "Where should this database's schema snapshot live? It holds one DDL file per table, view and function, ready to commit.",
      repository: "A Git repository",
      repositoryHelp:
        "Dolmen keeps its own copy and pushes every commit. Good for a repository just for the schema. It uses your git setup, so an SSH address such as git@github.com:you/schema.git pushes with your SSH key.",
      repositoryUrl: "Repository URL",
      folderInRepository: "Folder in it",
      useRepository: "Use repository",
      folder: "A folder on this computer",
      folderHelp: "For example db/schema in your app's repository, so schema and code change in the same commits.",
      chooseFolder: "Choose folder…",
    },

    changes: {
      comparing: "Comparing with the database…",
      matches: "The snapshot matches the database.",
      count: (n: number) => `${plural(n, "change", "changes")} since the last snapshot`,
      commitMessage: "Commit message",
      writeFiles: "Write files",
      updateDatabase: "Update database…",
      updateDatabaseTitle: "Change the database to match the files, for example after checking out an older commit",
      commit: "Commit",
      notInRepositoryTitle: "The folder isn't inside a Git repository",
      commitAndPush: "Commit & push",
      newCommitsArrived: "New commits arrived from the repository. Review the changes again before committing.",
      wrote: (files: number) => `Wrote ${plural(files, "file", "files")}`,
      committed: (hash: string, files: number, pushed: boolean) =>
        `${pushed ? "Committed and pushed" : "Committed"} ${hash} (${plural(files, "file", "files")})`,
      modelNote: "The schema model the DDL was written from. It lets Dolmen apply this snapshot to a database later.",
      defaultMessage: (database: string, lines: string[]) =>
        `Update ${database} schema snapshot\n\n${lines.join("\n")}`,
      messageLine: { added: "Add", modified: "Update", removed: "Remove" },
      andMore: (n: number) => `and ${n} more`,
    },

    history: {
      noCommits: "No commits yet.",
      comparingTo: (label: string) => `Database → ${label}`,
      comparisonHelp: "what would change to make the database match this commit",
      comparing: "Comparing…",
      alreadyMatches: "The database already matches this commit's snapshot.",
      updateDatabase: (label: string) => `Update database to match ${label}…`,
    },
  },

  apply: {
    title: (label: string) => `Update the database to match ${label}`,
    notLocal:
      "Only local databases (localhost, a loopback address or a Unix socket) can be changed from a snapshot. This connection's server is elsewhere, so the plan is shown for review only.",
    planning: "Planning…",
    blockersTitle: "Dolmen can't make these changes, so the plan can't be applied:",
    blockers: {
      enumValuesRemoved: (name: string) =>
        `Enum ${name}: removing or reordering values isn't supported; only adding values is.`,
      tableKindChanged: (name: string) => `Table ${name}: changing its kind or partitioning isn't supported.`,
      generatedColumnChanged: (name: string) =>
        `Column ${name}: turning it into a generated column, or changing whether it is stored, isn't supported.`,
      renameNoLongerApplies: (from: string, to: string) => `The rename of ${from} to ${to} no longer applies.`,
    },
    renamesIntro: "These drops look like renames. Renaming keeps the data:",
    renameTable: (from: string, to: string) => `Rename table ${from} → ${to}`,
    renameColumn: (from: string, to: string) => `Rename column ${from} → ${to}`,
    alreadyMatches: (label: string) => `The database already matches ${label}.`,
    steps: (n: number) =>
      `${plural(n, "step", "steps")}, run in one transaction: if any step fails, nothing changes.`,
    losesData: "Loses data",
    allowDestructive: (n: number) =>
      `Allow ${plural(n, "step", "steps")} that drop data or change column types. Dropped data can't be brought back by applying a newer snapshot.`,
    apply: "Apply",
    applying: "Applying…",
  },

  graph: {
    committedBy: (author: string, date: string) => `${author}, ${date}`,
  },

  // Labels in CodeMirror's search panel, keyed by its English phrases.
  editorPhrases: {} as Record<string, string>,
};

export type Messages = typeof en;
