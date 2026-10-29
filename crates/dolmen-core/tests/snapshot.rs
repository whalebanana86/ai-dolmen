//! Schema snapshots: files on disk, Git commits, and a round trip through PostgreSQL.

use std::path::Path;
use std::process::Command;

use dolmen_core::Error;
use dolmen_core::postgres::Session;
use dolmen_core::postgres::migrate;
use dolmen_core::schema::{Column, Database, EnumType, Schema, Table, TableKind, View};
use dolmen_core::snapshot::{ChangeKind, MARKER, MODEL_FILE, Snapshot};
use dolmen_core::{git, schema};
use tempfile::TempDir;

#[macro_use]
mod common;
use common::{exclusive, test_config};

fn column(name: &str, data_type: &str) -> Column {
    Column {
        name: name.into(),
        data_type: data_type.into(),
        nullable: true,
        default: None,
        identity: None,
        generated: None,
        comment: None,
    }
}

fn table(name: &str, columns: Vec<Column>) -> Table {
    Table {
        name: name.into(),
        kind: TableKind::Table,
        columns,
        constraints: vec![],
        indexes: vec![],
        partition_key: None,
        partition_of: None,
        grants: vec![],
        triggers: vec![],
        row_security: false,
        force_row_security: false,
        policies: vec![],
        comment: None,
    }
}

fn empty_schema(name: &str) -> Schema {
    Schema {
        name: name.into(),
        tables: vec![],
        views: vec![],
        functions: vec![],
        sequences: vec![],
        enums: vec![],
    }
}

/// `public` with a table and a view, and `app` with a table and an enum.
fn database() -> Database {
    Database {
        schemas: vec![
            Schema {
                tables: vec![table("people", vec![column("id", "bigint")])],
                enums: vec![EnumType {
                    name: "mood".into(),
                    labels: vec!["ok".into()],
                }],
                ..empty_schema("app")
            },
            Schema {
                tables: vec![table("users", vec![column("id", "bigint")])],
                views: vec![View {
                    name: "user_ids".into(),
                    materialized: false,
                    columns: vec![column("id", "bigint")],
                    definition: " SELECT id\n   FROM public.users;".into(),
                    comment: None,
                    indexes: vec![],
                    grants: vec![],
                    triggers: vec![],
                }],
                ..empty_schema("public")
            },
        ],
        extensions: vec![schema::Extension {
            name: "pgcrypto".into(),
            schema: "public".into(),
        }],
    }
}

fn paths(snapshot: &Snapshot) -> Vec<&str> {
    snapshot.files().keys().map(String::as_str).collect()
}

#[test]
fn lays_out_one_file_per_object() {
    assert_eq!(
        paths(&Snapshot::of(&database())),
        [
            "app/schema.sql",
            "app/tables/people.sql",
            "app/types/mood.sql",
            "extensions.sql",
            "public/tables/users.sql",
            "public/views/user_ids.sql",
        ]
    );
}

#[test]
fn writes_reads_and_replaces_snapshots() {
    let dir = TempDir::new().unwrap();
    let folder = dir.path().join("schema");
    let first = Snapshot::of(&database());
    first.write(&folder).unwrap();
    assert!(folder.join(MARKER).exists());
    assert!(folder.join(MODEL_FILE).exists());
    // Reading it back gives the same files and the same model.
    let read = Snapshot::read(&folder).unwrap();
    assert_eq!(read, first);
    assert_eq!(read.model(), Some(&database()));

    // Drop the app schema's enum and change the users table.
    let mut changed = database();
    changed.schemas[0].enums.clear();
    changed.schemas[1].tables[0]
        .columns
        .push(column("email", "text"));
    let second = Snapshot::of(&changed);

    let changes = first.changes(&second);
    assert_eq!(
        changes
            .iter()
            .map(|c| (c.path.as_str(), c.kind))
            .collect::<Vec<_>>(),
        [
            ("app/types/mood.sql", ChangeKind::Removed),
            ("public/tables/users.sql", ChangeKind::Modified),
        ]
    );
    assert!(
        changes[1].diff.contains("+    email text"),
        "{}",
        changes[1].diff
    );

    second.write(&folder).unwrap();
    assert_eq!(Snapshot::read(&folder).unwrap(), second);
    // The emptied types folder is gone too.
    assert!(!folder.join("app/types").exists());
    assert!(second.changes(&Snapshot::read(&folder).unwrap()).is_empty());
}

#[test]
fn reads_snapshots_written_before_models_and_rejects_newer_formats() {
    let dir = TempDir::new().unwrap();
    Snapshot::of(&database()).write(dir.path()).unwrap();

    std::fs::remove_file(dir.path().join(MODEL_FILE)).unwrap();
    let old = Snapshot::read(dir.path()).unwrap();
    assert_eq!(old.model(), None);
    assert!(!old.files().is_empty());

    std::fs::write(
        dir.path().join(MODEL_FILE),
        r#"{"format": 99, "database": {}}"#,
    )
    .unwrap();
    assert!(matches!(
        Snapshot::read(dir.path()),
        Err(Error::Storage { .. })
    ));
}

#[test]
fn refuses_folders_it_does_not_own() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "mine").unwrap();
    let err = Snapshot::of(&database()).write(dir.path()).unwrap_err();
    assert!(matches!(err, Error::Storage { .. }), "{err:?}");
    assert!(dir.path().join("notes.txt").exists());

    // A folder with only Finder's metadata counts as empty.
    let finder = TempDir::new().unwrap();
    std::fs::write(finder.path().join(".DS_Store"), "").unwrap();
    Snapshot::of(&database()).write(finder.path()).unwrap();
}

fn run_git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A repository that ignores the user's global hooks and signing settings.
fn repository() -> TempDir {
    let dir = TempDir::new().unwrap();
    run_git(dir.path(), &["init", "--quiet", "--initial-branch=main"]);
    configure(dir.path());
    dir
}

/// Sets a test identity and turns off the user's hooks and commit signing.
fn configure(repo: &Path) {
    let hooks = repo.join(".git").join("test-hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    for (key, value) in [
        ("user.name", "Dolmen Test"),
        ("user.email", "test@example.com"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", hooks.to_str().unwrap()),
    ] {
        run_git(repo, &["config", key, value]);
    }
}

#[test]
fn commits_only_the_snapshot_folder() {
    let repo = repository();
    let folder = repo.path().join("db").join("schema");

    let found = git::repository(&folder)
        .unwrap()
        .expect("inside a repository");
    assert_eq!(found.branch.as_deref(), Some("main"));
    assert_eq!(
        found.root.canonicalize().unwrap(),
        repo.path().canonicalize().unwrap()
    );

    // Unrelated staged work must stay out of the snapshot commit.
    std::fs::write(repo.path().join("notes.txt"), "work in progress").unwrap();
    run_git(repo.path(), &["add", "notes.txt"]);

    Snapshot::of(&database()).write(&folder).unwrap();
    let hash = git::commit(&folder, "Snapshot schema")
        .unwrap()
        .expect("a commit");
    assert_eq!(run_git(repo.path(), &["rev-parse", "HEAD"]), hash);
    let committed = run_git(repo.path(), &["show", "--name-only", "--format=", "HEAD"]);
    assert!(
        committed.lines().all(|path| path.starts_with("db/schema/")),
        "{committed}"
    );
    assert_eq!(
        run_git(repo.path(), &["diff", "--cached", "--name-only"]),
        "notes.txt"
    );

    // Nothing changed, so nothing to commit.
    assert_eq!(git::commit(&folder, "Again").unwrap(), None);
}

#[test]
fn pushes_and_tracks_the_upstream() {
    let remote = TempDir::new().unwrap();
    run_git(
        remote.path(),
        &["init", "--quiet", "--bare", "--initial-branch=main"],
    );
    let repo = repository();
    run_git(
        repo.path(),
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    let folder = repo.path().join("db").join("schema");

    Snapshot::of(&database()).write(&folder).unwrap();
    git::commit(&folder, "First snapshot").unwrap();
    let before = git::repository(&folder).unwrap().unwrap();
    assert_eq!(
        (before.upstream, before.remotes),
        (None, vec!["origin".to_owned()])
    );

    // The first push sets the upstream.
    git::push(&folder).unwrap();
    let pushed = git::repository(&folder).unwrap().unwrap();
    assert_eq!(pushed.upstream.as_deref(), Some("origin/main"));
    assert_eq!((pushed.ahead, pushed.behind), (0, 0));
    assert_eq!(
        run_git(remote.path(), &["rev-parse", "main"]),
        run_git(repo.path(), &["rev-parse", "HEAD"])
    );

    let mut changed = database();
    changed.schemas[0].enums.clear();
    Snapshot::of(&changed).write(&folder).unwrap();
    git::commit(&folder, "Drop mood").unwrap();
    assert_eq!(git::repository(&folder).unwrap().unwrap().ahead, 1);
    git::push(&folder).unwrap();
    assert_eq!(git::repository(&folder).unwrap().unwrap().ahead, 0);
}

#[test]
fn reads_history_and_past_snapshots() {
    let repo = repository();
    let folder = repo.path().join("db").join("schema");
    assert!(
        git::history(repo.path(), 50).unwrap().is_empty(),
        "no commits yet"
    );

    let first = Snapshot::of(&database());
    first.write(&folder).unwrap();
    git::commit(&folder, "First snapshot").unwrap();
    run_git(repo.path(), &["tag", "v1"]);

    let mut changed = database();
    changed.schemas[1].tables[0]
        .columns
        .push(column("email", "text"));
    let second = Snapshot::of(&changed);
    second.write(&folder).unwrap();
    git::commit(&folder, "Add email").unwrap();

    std::fs::write(repo.path().join("README.md"), "docs").unwrap();
    run_git(repo.path(), &["add", "README.md"]);
    run_git(
        repo.path(),
        &["commit", "--quiet", "--message", "Write docs"],
    );

    let history = git::history(&folder, 50).unwrap();
    assert_eq!(
        history
            .iter()
            .map(|c| (c.subject.as_str(), c.changes_snapshot, c.head))
            .collect::<Vec<_>>(),
        [
            ("Write docs", false, true),
            ("Add email", true, false),
            ("First snapshot", true, false),
        ]
    );
    assert_eq!(history[0].refs, ["main"]);
    assert_eq!(history[2].refs, ["tag: v1"]);
    assert_eq!(history[0].parents, [history[1].hash.clone()]);
    assert!(history[2].parents.is_empty());

    assert_eq!(git::snapshot_at(&folder, "v1").unwrap(), first);
    assert_eq!(
        git::snapshot_at(&folder, "v1").unwrap().model(),
        Some(&database())
    );
    assert_eq!(git::snapshot_at(&folder, "HEAD").unwrap(), second);
    assert!(git::snapshot_at(&folder, "--output=/tmp/x").is_err());
}

#[test]
fn clones_syncs_and_pushes_a_schema_repository() {
    // An empty remote, like a new GitHub repository.
    let remote = TempDir::new().unwrap();
    run_git(
        remote.path(),
        &["init", "--quiet", "--bare", "--initial-branch=main"],
    );
    let url = remote.path().to_str().unwrap();

    let home = TempDir::new().unwrap();
    let clone = home
        .path()
        .join("repositories")
        .join(git::clone_dir_name(url));
    git::clone(url, &clone).unwrap();
    configure(&clone);
    assert_eq!(git::origin_url(&clone).unwrap().as_deref(), Some(url));
    assert!(
        !git::sync(&clone).unwrap(),
        "nothing to sync before the first push"
    );

    let folder = clone.join("db").join("schema");
    Snapshot::of(&database()).write(&folder).unwrap();
    git::commit(&folder, "First snapshot").unwrap();
    git::push(&folder).unwrap();

    // A teammate pushes a change from their own clone.
    let teammate = TempDir::new().unwrap();
    let theirs = teammate.path().join("schema-repo");
    git::clone(url, &theirs).unwrap();
    configure(&theirs);
    let mut changed = database();
    changed.schemas[0].enums.clear();
    Snapshot::of(&changed)
        .write(&theirs.join("db").join("schema"))
        .unwrap();
    git::commit(&theirs.join("db").join("schema"), "Drop mood").unwrap();
    git::push(&theirs).unwrap();

    assert!(git::sync(&clone).unwrap(), "the teammate's commit arrives");
    assert_eq!(Snapshot::read(&folder).unwrap(), Snapshot::of(&changed));
    assert!(!git::sync(&clone).unwrap());

    // Another address for the same repository keeps the clone.
    let other_address = format!("{url}/");
    assert!(git::same_repository(url, &other_address));
    git::set_origin_url(&clone, &other_address).unwrap();
    assert_eq!(
        git::origin_url(&clone).unwrap().as_deref(),
        Some(other_address.as_str())
    );
    assert!(!git::sync(&clone).unwrap());

    assert!(git::clone("--upload-pack=touch", &home.path().join("x")).is_err());
    assert!(git::set_origin_url(&clone, "--upload-pack=touch").is_err());
}

#[test]
fn creates_and_switches_branches() {
    let repo = repository();
    let folder = repo.path().join("db").join("schema");
    Snapshot::of(&database()).write(&folder).unwrap();
    git::commit(&folder, "First snapshot").unwrap();

    git::create_branch(&folder, "drop-mood").unwrap();
    let mut changed = database();
    changed.schemas[0].enums.clear();
    Snapshot::of(&changed).write(&folder).unwrap();
    git::commit(&folder, "Drop mood").unwrap();
    let repository = git::repository(&folder).unwrap().unwrap();
    assert_eq!(repository.branch.as_deref(), Some("drop-mood"));
    assert_eq!(repository.branches, ["drop-mood", "main"]);

    // The files follow the branch, like code.
    git::switch_branch(&folder, "main").unwrap();
    assert_eq!(Snapshot::read(&folder).unwrap(), Snapshot::of(&database()));
    git::switch_branch(&folder, "drop-mood").unwrap();
    assert_eq!(Snapshot::read(&folder).unwrap(), Snapshot::of(&changed));

    // Uncommitted snapshot files would be carried to the other branch.
    Snapshot::of(&database()).write(&folder).unwrap();
    assert!(matches!(
        git::switch_branch(&folder, "main"),
        Err(Error::Refused {
            reason: dolmen_core::Refusal::UncommittedChanges,
            ..
        })
    ));
    assert_eq!(
        git::repository(&folder).unwrap().unwrap().branch.as_deref(),
        Some("drop-mood")
    );

    for name in ["-f", "a..b", "with space", ""] {
        assert!(git::create_branch(&folder, name).is_err(), "{name:?}");
    }
}

#[test]
fn switches_to_branches_that_exist_only_on_the_remote() {
    let remote = TempDir::new().unwrap();
    run_git(
        remote.path(),
        &["init", "--quiet", "--bare", "--initial-branch=main"],
    );
    let url = remote.path().to_str().unwrap();
    let home = TempDir::new().unwrap();
    let mine = home.path().join("mine");
    git::clone(url, &mine).unwrap();
    configure(&mine);
    let folder = mine.join("db").join("schema");
    Snapshot::of(&database()).write(&folder).unwrap();
    git::commit(&folder, "First snapshot").unwrap();
    git::push(&folder).unwrap();
    git::create_branch(&folder, "feature").unwrap();
    git::push(&folder).unwrap();

    let theirs = home.path().join("theirs");
    git::clone(url, &theirs).unwrap();
    let folder = theirs.join("db").join("schema");
    assert_eq!(
        git::repository(&folder).unwrap().unwrap().branches,
        ["main", "feature"]
    );
    git::switch_branch(&folder, "feature").unwrap();
    let repository = git::repository(&folder).unwrap().unwrap();
    assert_eq!(repository.branch.as_deref(), Some("feature"));
    assert_eq!(repository.upstream.as_deref(), Some("origin/feature"));
}

#[test]
fn finds_no_repository_outside_git() {
    let dir = TempDir::new().unwrap();
    assert_eq!(git::repository(&dir.path().join("missing")).unwrap(), None);
}

fn schema_named<'a>(database: &'a Database, name: &str) -> &'a Schema {
    database
        .schemas
        .iter()
        .find(|s| s.name == name)
        .expect("schema listed")
}

#[tokio::test]
async fn snapshot_files_recreate_the_same_schema() {
    let config = config_or_skip!();
    let _exclusive = exclusive().await;
    let session = Session::connect(&config).await.expect("connect");
    session
        .execute(
            "DROP SCHEMA IF EXISTS roundtrip_test CASCADE;
             CREATE SCHEMA roundtrip_test;
             CREATE TYPE roundtrip_test.mood AS ENUM ('sad', 'ok', 'happy');
             CREATE SEQUENCE roundtrip_test.invoice_numbers;
             CREATE TABLE roundtrip_test.authors (
               id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
               email varchar(255) NOT NULL UNIQUE,
               mood roundtrip_test.mood DEFAULT 'ok'
             );
             COMMENT ON TABLE roundtrip_test.authors IS 'people who write';
             COMMENT ON COLUMN roundtrip_test.authors.email IS 'it''s their login';
             CREATE TABLE roundtrip_test.posts (
               id serial PRIMARY KEY,
               author_id bigint NOT NULL REFERENCES roundtrip_test.authors (id),
               title text NOT NULL CHECK (title <> ''),
               title_length int GENERATED ALWAYS AS (length(title)) STORED
             );
             CREATE INDEX posts_author_idx ON roundtrip_test.posts (author_id);
             CREATE TABLE roundtrip_test.events (at timestamptz NOT NULL) PARTITION BY RANGE (at);
             CREATE VIEW roundtrip_test.recent_posts AS SELECT id, title FROM roundtrip_test.posts;
             CREATE MATERIALIZED VIEW roundtrip_test.post_counts AS
               SELECT author_id, count(*) AS n FROM roundtrip_test.posts GROUP BY author_id;
             CREATE FUNCTION roundtrip_test.add(a int, b int) RETURNS int LANGUAGE sql AS 'SELECT a + b';
             CREATE FUNCTION roundtrip_test.add(a text, b text) RETURNS text LANGUAGE sql AS 'SELECT a || b';",
            0,
        )
        .await
        .expect("create the schema");

    let before = session.introspect().await.expect("introspect");
    let snapshot = Snapshot::of(&before);

    // Recreate the schema from nothing but its snapshot files, in dependency order.
    session
        .execute("DROP SCHEMA roundtrip_test CASCADE", 0)
        .await
        .unwrap();
    for kind in [
        "schema.sql",
        "types/",
        "sequences/",
        "tables/",
        "views/",
        "functions/",
    ] {
        let prefix = format!("roundtrip_test/{kind}");
        for (path, sql) in snapshot
            .files()
            .iter()
            .filter(|(path, _)| path.starts_with(&prefix))
        {
            session
                .execute(sql, 0)
                .await
                .unwrap_or_else(|err| panic!("{path}: {err}\n{sql}"));
        }
    }

    let after = session.introspect().await.expect("introspect again");
    session
        .execute("DROP SCHEMA roundtrip_test CASCADE", 0)
        .await
        .unwrap();

    assert_eq!(
        schema_named(&after, "roundtrip_test"),
        schema_named(&before, "roundtrip_test")
    );
    assert!(snapshot.files()["roundtrip_test/tables/posts.sql"].contains("id serial NOT NULL"));
    assert_eq!(
        snapshot
            .files()
            .keys()
            .filter(|p| p.starts_with("roundtrip_test/functions/"))
            .count(),
        1,
        "both overloads of add share a file"
    );
}

/// Files of one schema, so tests running at the same time don't see each other's schemas.
fn files_of(snapshot: &Snapshot, schema: &str) -> Vec<(String, String)> {
    let prefix = format!("{schema}/");
    snapshot
        .files()
        .iter()
        .filter(|(path, _)| path.starts_with(&prefix))
        .map(|(path, sql)| (path.clone(), sql.clone()))
        .collect()
}

#[tokio::test]
async fn snapshots_ignore_the_sessions_search_path() {
    let config = config_or_skip!();
    let _exclusive = exclusive().await;
    let session = Session::connect(&config).await.expect("connect");
    session
        .execute(
            "DROP SCHEMA IF EXISTS search_path_test CASCADE;
             CREATE SCHEMA search_path_test;
             CREATE TYPE search_path_test.mood AS ENUM ('ok');
             CREATE TABLE search_path_test.a (id int PRIMARY KEY, mood search_path_test.mood DEFAULT 'ok');
             CREATE TABLE search_path_test.b (a_id int REFERENCES search_path_test.a (id));
             CREATE VIEW search_path_test.v AS SELECT id FROM search_path_test.a;",
            0,
        )
        .await
        .expect("create the schema");
    let plain = Snapshot::of(&session.introspect().await.unwrap());

    // With the schema on the user's search_path, definitions could drop the
    // schema name; the snapshot must not change.
    let other = Session::connect(&config).await.expect("connect");
    other
        .execute("SET search_path = search_path_test, public", 0)
        .await
        .unwrap();
    let with_search_path = Snapshot::of(&other.introspect().await.unwrap());
    session
        .execute("DROP SCHEMA search_path_test CASCADE", 0)
        .await
        .unwrap();

    let files = files_of(&plain, "search_path_test");
    assert_eq!(files.len(), 5, "{files:?}");
    assert_eq!(files_of(&with_search_path, "search_path_test"), files);
    assert!(
        files
            .iter()
            .any(|(_, sql)| sql.contains("REFERENCES search_path_test.a(id)"))
    );
}

/// The table's columns and their types, as the database reports them.
async fn column_types(session: &Session, schema: &str, table: &str) -> Vec<(String, String)> {
    let result = session
        .execute(
            &format!(
                "SELECT column_name::text, format_type(atttypid, atttypmod)
                 FROM information_schema.columns c
                 JOIN pg_attribute a ON a.attrelid = '{schema}.{table}'::regclass AND a.attname = c.column_name
                 WHERE table_schema = '{schema}' AND table_name = '{table}'
                 ORDER BY ordinal_position"
            ),
            100,
        )
        .await
        .unwrap();
    match &result[0] {
        dolmen_core::postgres::StatementResult::Rows { rows, .. } => rows
            .iter()
            .map(|row| (row[0].clone().unwrap(), row[1].clone().unwrap()))
            .collect(),
        other => panic!("{other:?}"),
    }
}

/// Like code: change a column on a branch and commit it, switch back to main,
/// and the database follows the branch's snapshot, keeping the rows.
#[tokio::test]
async fn databases_follow_branch_switches() {
    let config = config_or_skip!();
    let _exclusive = exclusive().await;
    let session = Session::connect(&config).await.expect("connect");
    const SCHEMA: &str = "branch_test";
    let model = || async {
        let mut database = session.introspect().await.unwrap();
        database.schemas.retain(|s| s.name == SCHEMA);
        database.extensions.clear();
        database
    };
    let apply_files = |folder: std::path::PathBuf| {
        let model = &model;
        let session = &session;
        async move {
            let target = Snapshot::read(&folder).unwrap().model().unwrap().clone();
            let plan = migrate::plan(&model().await, &target, &[]);
            assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
            session.apply(&plan).await.unwrap();
            plan
        }
    };
    session
        .execute(
            &format!(
                "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE;
                 CREATE SCHEMA {SCHEMA};
                 CREATE TABLE {SCHEMA}.users (id int PRIMARY KEY, name varchar(50) NOT NULL);
                 INSERT INTO {SCHEMA}.users VALUES (1, 'kim'), (2, 'lee');"
            ),
            0,
        )
        .await
        .unwrap();
    let repo = repository();
    let folder = repo.path().join("db").join("schema");
    Snapshot::of(&model().await).write(&folder).unwrap();
    git::commit(&folder, "Users").unwrap();
    let before = column_types(&session, SCHEMA, "users").await;

    // On a branch: widen a column, add one, and commit the snapshot.
    git::create_branch(&folder, "widen-name").unwrap();
    session
        .execute(
            &format!(
                "ALTER TABLE {SCHEMA}.users ALTER COLUMN name TYPE varchar(200);
                 ALTER TABLE {SCHEMA}.users ADD COLUMN email text"
            ),
            0,
        )
        .await
        .unwrap();
    Snapshot::of(&model().await).write(&folder).unwrap();
    git::commit(&folder, "Widen name, add email").unwrap();
    let after = column_types(&session, SCHEMA, "users").await;
    assert_ne!(before, after);

    // Back on main, the database follows the files.
    git::switch_branch(&folder, "main").unwrap();
    let back = apply_files(folder.clone()).await;
    assert!(back.is_destructive(), "dropping email loses its data");
    assert_eq!(column_types(&session, SCHEMA, "users").await, before);
    let rows = session
        .execute(&format!("SELECT count(*) FROM {SCHEMA}.users"), 1)
        .await
        .unwrap();
    assert!(
        matches!(&rows[0], dolmen_core::postgres::StatementResult::Rows { rows, .. } if rows[0][0].as_deref() == Some("2"))
    );

    // And forward again.
    git::switch_branch(&folder, "widen-name").unwrap();
    apply_files(folder.clone()).await;
    assert_eq!(column_types(&session, SCHEMA, "users").await, after);
    assert!(apply_files(folder.clone()).await.steps.is_empty());

    session
        .execute(&format!("DROP SCHEMA {SCHEMA} CASCADE"), 0)
        .await
        .unwrap();
}

#[cfg(unix)]
#[test]
fn refuses_links_and_leaves_repositories_in_the_folder_alone() {
    let outside = TempDir::new().unwrap();
    let victim = outside.path().join("important.sql");
    std::fs::write(&victim, "keep me").unwrap();

    let dir = TempDir::new().unwrap();
    let folder = dir.path().join("schema");
    Snapshot::of(&database()).write(&folder).unwrap();
    // A link to a file outside, under a name the snapshot writes.
    let users = folder.join("public").join("tables").join("users.sql");
    std::fs::remove_file(&users).unwrap();
    std::os::unix::fs::symlink(&victim, &users).unwrap();
    let err = Snapshot::of(&database()).write(&folder).unwrap_err();
    assert!(err.to_string().contains("symbolic link"), "{err}");
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me");

    // A linked folder is refused too, without deleting what's in it.
    std::fs::remove_file(&users).unwrap();
    std::os::unix::fs::symlink(outside.path(), folder.join("loot")).unwrap();
    assert!(Snapshot::of(&database()).write(&folder).is_err());
    assert!(victim.exists());
    std::fs::remove_file(folder.join("loot")).unwrap();

    // A repository made right in the snapshot folder survives writes.
    run_git(&folder, &["init", "--quiet"]);
    Snapshot::of(&database()).write(&folder).unwrap();
    Snapshot::of(&database()).write(&folder).unwrap();
    run_git(&folder, &["status", "--short"]);
}
