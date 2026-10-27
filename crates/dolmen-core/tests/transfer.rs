//! Exporting and importing table data against a real PostgreSQL server.
//!
//! Skipped unless `DOLMEN_TEST_PG_HOST` is set.

use std::path::PathBuf;

use dolmen_core::Error;
use dolmen_core::postgres::transfer::{
    CsvOptions, Delimiter, Encoding, Format, Source, Transferred, preview_csv,
};
use dolmen_core::postgres::{Session, StatementResult};

#[macro_use]
mod common;
use common::{exclusive, test_config};

/// A file in a fresh temporary folder.
fn temp_file(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dolmen-transfer-{}", uuid_like()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

async fn rows(session: &Session, sql: &str) -> Vec<Vec<Option<String>>> {
    match session.execute(sql, 1000).await.unwrap().pop().unwrap() {
        StatementResult::Rows { rows, .. } => rows,
        other => panic!("expected rows, got {other:?}"),
    }
}

fn table(name: &str) -> Source {
    Source::Table {
        schema: "transfer_test".into(),
        name: name.into(),
    }
}

async fn setup(session: &Session) {
    session
        .execute(
            "DROP SCHEMA IF EXISTS transfer_test CASCADE; CREATE SCHEMA transfer_test;
             CREATE TABLE transfer_test.people (
                 id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
                 name text,
                 note text,
                 born date,
                 tags text[],
                 name_length int GENERATED ALWAYS AS (length(name)) STORED
             );
             INSERT INTO transfer_test.people (name, note, born, tags) VALUES
                 ('홍길동', NULL, '1990-01-02', '{a,b}'),
                 ('O''Brien', '', NULL, NULL),
                 ('comma, \"quote\"', E'two\\nlines', '2000-12-31', '{}');",
            10,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn exports_csv_as_copy_writes_it() {
    let config = config_or_skip!();
    let _exclusive = exclusive().await;
    let session = Session::connect(&config).await.unwrap();
    setup(&session).await;
    let path = temp_file("people.csv");
    let format = Format::Csv {
        delimiter: Delimiter::Comma,
        header: true,
        bom: true,
    };
    let done = session
        .export(&table("people"), &format, &path, |_| {})
        .await
        .unwrap();
    assert_eq!(done.rows, 3);
    let written = std::fs::read(&path).unwrap();
    assert_eq!(done.bytes, written.len() as u64);
    let text = String::from_utf8(written).unwrap();
    assert_eq!(
        text,
        "\u{feff}id,name,note,born,tags,name_length\n\
         1,홍길동,,1990-01-02,\"{a,b}\",3\n\
         2,O'Brien,\"\",,,7\n\
         3,\"comma, \"\"quote\"\"\",\"two\nlines\",2000-12-31,{},14\n"
    );
}

#[tokio::test]
async fn exports_json_and_queries_read_only() {
    let config = config_or_skip!();
    let _exclusive = exclusive().await;
    let session = Session::connect(&config).await.unwrap();
    setup(&session).await;
    let path = temp_file("people.json");
    // Duplicate column names and a trailing comment still work.
    let query = Source::Query {
        sql: "SELECT id, name AS v, born AS v FROM transfer_test.people WHERE id < 3 ORDER BY id; -- two".into(),
    };
    let done = session
        .export(&query, &Format::Json, &path, |_| {})
        .await
        .unwrap();
    assert_eq!(done.rows, 2);
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(json[0]["id"], 1);
    assert_eq!(json[1]["v"], serde_json::Value::Null);
    assert_eq!(json.as_array().unwrap().len(), 2);

    // Exports never change data: a statement that would is refused.
    let delete = Source::Query {
        sql: "DELETE FROM transfer_test.people RETURNING id".into(),
    };
    assert!(
        session
            .export(&delete, &Format::Json, &path, |_| {})
            .await
            .is_err()
    );
    assert_eq!(
        rows(&session, "SELECT count(*) FROM transfer_test.people").await,
        [vec![Some("3".into())]]
    );
    // A statement without columns has nothing to export.
    match session
        .export(
            &Source::Query {
                sql: "SELECT".into(),
            },
            &Format::Json,
            &path,
            |_| {},
        )
        .await
    {
        Err(Error::Refused { .. }) => {}
        other => panic!("expected a refusal, got {other:?}"),
    }
    // An empty result is an empty array.
    let none = Source::Query {
        sql: "SELECT 1 AS n WHERE false".into(),
    };
    session
        .export(&none, &Format::Json, &path, |_| {})
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[]\n");
}

#[tokio::test]
async fn insert_statements_load_back_the_same_rows() {
    let config = config_or_skip!();
    let _exclusive = exclusive().await;
    let session = Session::connect(&config).await.unwrap();
    setup(&session).await;
    let path = temp_file("people.sql");
    let format = Format::Insert {
        table: "transfer_test.people".into(),
    };
    session
        .export(&table("people"), &format, &path, |_| {})
        .await
        .unwrap();
    let sql = std::fs::read_to_string(&path).unwrap();
    // The generated column is left out; the identity ALWAYS one is overridden.
    assert!(
        sql.starts_with(
            "INSERT INTO transfer_test.people (id, name, note, born, tags) OVERRIDING SYSTEM VALUE VALUES ('1', '홍길동', NULL, '1990-01-02', '{a,b}');"
        ),
        "{sql}"
    );
    let before = rows(&session, "SELECT * FROM transfer_test.people ORDER BY id").await;
    session
        .execute(&format!("TRUNCATE transfer_test.people; {sql}"), 10)
        .await
        .unwrap();
    assert_eq!(
        rows(&session, "SELECT * FROM transfer_test.people ORDER BY id").await,
        before
    );
}

#[tokio::test]
async fn csv_round_trips_through_import() {
    let config = config_or_skip!();
    let _exclusive = exclusive().await;
    let session = Session::connect(&config).await.unwrap();
    setup(&session).await;
    let path = temp_file("people.csv");
    let format = Format::Csv {
        delimiter: Delimiter::Tab,
        header: true,
        bom: true,
    };
    session
        .export(&table("people"), &format, &path, |_| {})
        .await
        .unwrap();
    let preview = preview_csv(&path, None, Encoding::Utf8, 10).unwrap();
    assert_eq!(preview.delimiter, Delimiter::Tab);
    assert_eq!(preview.rows[0][0], "id", "the byte order mark is dropped");
    assert!(!preview.looks_euc_kr);

    let before = rows(
        &session,
        "SELECT id, name, note, born, tags FROM transfer_test.people ORDER BY id",
    )
    .await;
    session
        .execute("TRUNCATE transfer_test.people", 10)
        .await
        .unwrap();
    let columns = ["id", "name", "note", "born", "tags"]
        .map(|c| Some(c.to_owned()))
        .into_iter()
        // The generated column is read from the file but not written.
        .chain([None])
        .collect::<Vec<_>>();
    let options = CsvOptions {
        delimiter: Delimiter::Tab,
        header: true,
        encoding: Encoding::Utf8,
    };
    let done = session
        .import_csv("transfer_test", "people", &path, options, &columns, |_| {})
        .await
        .unwrap();
    assert_eq!(done.rows, 3);
    let after = rows(
        &session,
        "SELECT id, name, note, born, tags FROM transfer_test.people ORDER BY id",
    )
    .await;
    // Skipping a column rewrites the rows, which reads an empty string as NULL.
    let mut expected = before;
    expected[1][2] = None;
    assert_eq!(after, expected);
}

#[tokio::test]
async fn imports_euc_kr_files_as_excel_saves_them() {
    let config = config_or_skip!();
    let _exclusive = exclusive().await;
    let session = Session::connect(&config).await.unwrap();
    setup(&session).await;
    let path = temp_file("excel.csv");
    let (bytes, _, _) = encoding_rs::EUC_KR.encode("이름,메모\n김철수,\"안녕, 하세요\"\n이영희,\n");
    std::fs::write(&path, &bytes).unwrap();
    let preview = preview_csv(&path, None, Encoding::Utf8, 10).unwrap();
    assert!(preview.looks_euc_kr);
    let preview = preview_csv(&path, None, Encoding::EucKr, 10).unwrap();
    assert_eq!(preview.rows[1], ["김철수", "안녕, 하세요"]);

    session
        .execute("TRUNCATE transfer_test.people", 10)
        .await
        .unwrap();
    let options = CsvOptions {
        delimiter: Delimiter::Comma,
        header: true,
        encoding: Encoding::EucKr,
    };
    let columns = [Some("name".to_owned()), Some("note".to_owned())];
    let done: Transferred = session
        .import_csv("transfer_test", "people", &path, options, &columns, |_| {})
        .await
        .unwrap();
    assert_eq!(done.rows, 2);
    assert_eq!(done.bytes, bytes.len() as u64);
    assert_eq!(
        rows(
            &session,
            "SELECT name, note FROM transfer_test.people ORDER BY name"
        )
        .await,
        [
            vec![Some("김철수".into()), Some("안녕, 하세요".into())],
            vec![Some("이영희".into()), None],
        ]
    );

    // Read as UTF-8, the same file is refused rather than imported garbled.
    let utf8 = CsvOptions {
        encoding: Encoding::Utf8,
        ..options
    };
    assert!(
        session
            .import_csv("transfer_test", "people", &path, utf8, &columns, |_| {})
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_bad_value_names_its_line_and_imports_nothing() {
    let config = config_or_skip!();
    let _exclusive = exclusive().await;
    let session = Session::connect(&config).await.unwrap();
    setup(&session).await;
    let path = temp_file("bad.csv");
    std::fs::write(&path, "name,born\nok,2001-01-01\nbad,not a date\n").unwrap();
    let options = CsvOptions {
        delimiter: Delimiter::Comma,
        header: true,
        encoding: Encoding::Utf8,
    };
    let columns = [Some("name".to_owned()), Some("born".to_owned())];
    match session
        .import_csv("transfer_test", "people", &path, options, &columns, |_| {})
        .await
    {
        Err(Error::Database { code, detail, .. }) => {
            assert_eq!(code, "22007");
            let detail = detail.unwrap();
            assert!(detail.contains("line 3"), "{detail}");
        }
        other => panic!("expected a database error, got {other:?}"),
    }
    assert_eq!(
        rows(&session, "SELECT count(*) FROM transfer_test.people").await,
        [vec![Some("3".into())]]
    );
    // The session is usable afterwards.
    rows(&session, "SELECT 1").await;
    session
        .execute("DROP SCHEMA transfer_test CASCADE", 10)
        .await
        .unwrap();
}
