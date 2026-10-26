//! Schema snapshots: files on disk, Git commits, and a round trip through PostgreSQL.

use std::path::Path;
use std::process::Command;

use dolmen_core::Error;
use dolmen_core::postgres::Session;
use dolmen_core::schema::{Column, Database, EnumType, Schema, Table, TableKind, View};
use dolmen_core::snapshot::{ChangeKind, MARKER, Snapshot};
use dolmen_core::{git, schema};
use tempfile::TempDir;

#[macro_use]
mod common;
use common::test_config;

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
    assert_eq!(Snapshot::read(&folder).unwrap(), first);

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
    let hooks = dir.path().join(".git-test-hooks");
    std::fs::create_dir(&hooks).unwrap();
    run_git(dir.path(), &["init", "--quiet", "--initial-branch=main"]);
    for (key, value) in [
        ("user.name", "Dolmen Test"),
        ("user.email", "test@example.com"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", hooks.to_str().unwrap()),
    ] {
        run_git(dir.path(), &["config", key, value]);
    }
    dir
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
