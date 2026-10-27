//! Table data in and out: exporting rows to CSV, JSON or INSERT statements,
//! and importing CSV files with COPY.
//!
//! Both are meant for a session of their own: an export holds a read-only
//! transaction for its whole run, and an import commits or rolls back as one.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use encoding_rs::EUC_KR;
use encoding_rs_io::DecodeReaderBytesBuilder;
use futures_util::{SinkExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio_postgres::types::ToSql;

use super::Session;
use super::ddl::quote_ident;
use crate::{Error, Refusal};

/// Where exported rows come from.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Source {
    Table {
        schema: String,
        name: String,
    },
    /// One statement that returns rows. It runs read-only.
    Query {
        sql: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Delimiter {
    Comma,
    Tab,
    Semicolon,
    Pipe,
}

impl Delimiter {
    const ALL: [Delimiter; 4] = [
        Delimiter::Comma,
        Delimiter::Tab,
        Delimiter::Semicolon,
        Delimiter::Pipe,
    ];

    fn byte(self) -> u8 {
        match self {
            Delimiter::Comma => b',',
            Delimiter::Tab => b'\t',
            Delimiter::Semicolon => b';',
            Delimiter::Pipe => b'|',
        }
    }

    fn literal(self) -> &'static str {
        match self {
            Delimiter::Comma => "','",
            Delimiter::Tab => "E'\\t'",
            Delimiter::Semicolon => "';'",
            Delimiter::Pipe => "'|'",
        }
    }
}

/// How exported rows are written.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Format {
    /// NULL is an empty field and an empty string `""`, as COPY reads them.
    Csv {
        delimiter: Delimiter,
        header: bool,
        /// Starts with a UTF-8 byte order mark, so Excel reads the file as UTF-8.
        bom: bool,
    },
    /// A JSON array with an object per row.
    Json,
    /// An INSERT statement per row into `table`, written as given.
    Insert { table: String },
}

/// What an export or import moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Transferred {
    pub rows: u64,
    /// Bytes written to the file, or read from it.
    pub bytes: u64,
}

/// The text encoding of a file to import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Encoding {
    Utf8,
    /// EUC-KR with Microsoft's extensions (CP949), as Excel saves CSV on Korean Windows.
    EucKr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CsvOptions {
    pub delimiter: Delimiter,
    /// The first row names the columns rather than holding data.
    pub header: bool,
    pub encoding: Encoding,
}

/// The start of a file to import, to choose its columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    /// The delimiter used to read it: the one asked for, or the likeliest.
    pub delimiter: Delimiter,
    /// The first rows as read, the header row included.
    pub rows: Vec<Vec<String>>,
    /// Read as UTF-8 it has invalid bytes, but it reads as EUC-KR.
    pub looks_euc_kr: bool,
    pub size: u64,
}

/// Rows between progress reports.
const PROGRESS_ROWS: u64 = 1000;
const CHUNK: usize = 64 * 1024;

impl Session {
    /// Writes the rows of `source` to a new file at `path`, reporting the rows
    /// written so far. Runs in a read-only transaction, so a query can't
    /// change anything, and all rows come from one snapshot.
    pub async fn export(
        &self,
        source: &Source,
        format: &Format,
        path: &Path,
        mut progress: impl FnMut(u64),
    ) -> Result<Transferred, Error> {
        self.client
            .batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .await?;
        // Written beside the file and renamed when complete, so a failed or
        // cancelled export neither leaves half a file nor loses the one replaced.
        let partial = partial_path(path);
        let result = self
            .export_rows(source, format, &partial, &mut progress)
            .await;
        // Nothing to keep: the transaction only read.
        let _ = self.client.batch_execute("ROLLBACK").await;
        match result {
            Ok(done) => {
                tokio::fs::rename(&partial, path)
                    .await
                    .map_err(file_error(path))?;
                Ok(done)
            }
            Err(err) => {
                let _ = tokio::fs::remove_file(&partial).await;
                Err(err)
            }
        }
    }

    async fn export_rows(
        &self,
        source: &Source,
        format: &Format,
        path: &Path,
        progress: &mut impl FnMut(u64),
    ) -> Result<Transferred, Error> {
        let relation = match source {
            Source::Table { schema, name } => {
                format!("{}.{}", quote_ident(schema), quote_ident(name))
            }
            // On lines of their own, so a comment can't swallow the parenthesis.
            Source::Query { sql } => format!("(\n{}\n)", crate::sql::without_terminator(sql)),
        };
        // The extended protocol takes one statement, so nothing else rides along.
        let described = self
            .client
            .prepare(&format!("SELECT * FROM {relation} AS t"))
            .await?;
        let names: Vec<String> = described
            .columns()
            .iter()
            .map(|c| c.name().to_owned())
            .collect();
        if names.is_empty() {
            return Err(Error::Refused {
                reason: Refusal::NoColumns,
                message: "The statement returns no columns to export.".into(),
            });
        }
        // Columns are renamed by position, so duplicate names in a query still work.
        let aliases: Vec<String> = (1..=names.len()).map(|i| format!("c{i}")).collect();
        let renamed = format!("{relation} AS t({})", aliases.join(", "));

        let (query, columns, overriding) = match format {
            Format::Json => (
                format!("SELECT row_to_json(t)::text FROM {relation} AS t"),
                Vec::new(),
                false,
            ),
            Format::Csv { .. } => (text_select(&aliases, &renamed, None), names.clone(), false),
            Format::Insert { .. } => {
                // Generated columns can't be given values; identity ALWAYS ones need OVERRIDING.
                let (kept, overriding) = match source {
                    Source::Table { .. } => self.insertable(&relation, names.len()).await?,
                    Source::Query { .. } => ((0..names.len()).collect(), false),
                };
                let columns = kept.iter().map(|&i| names[i].clone()).collect();
                (
                    text_select(&aliases, &renamed, Some(&kept)),
                    columns,
                    overriding,
                )
            }
        };

        let file = tokio::fs::File::create(path)
            .await
            .map_err(file_error(path))?;
        let mut out = Output {
            writer: BufWriter::new(file),
            path,
            bytes: 0,
        };
        let mut buffer = Vec::with_capacity(CHUNK);
        match format {
            Format::Csv {
                delimiter,
                header,
                bom,
            } => {
                if *bom {
                    buffer.extend_from_slice(b"\xEF\xBB\xBF");
                }
                if *header {
                    let fields = columns.iter().map(|name| Some(name.as_bytes()));
                    csv_record(&mut buffer, fields, delimiter.byte());
                }
            }
            Format::Json => buffer.push(b'['),
            Format::Insert { .. } => {}
        }
        let insert_into = match format {
            Format::Insert { table } => {
                let list: Vec<String> = columns.iter().map(|c| quote_ident(c)).collect();
                let overriding = if overriding {
                    " OVERRIDING SYSTEM VALUE"
                } else {
                    ""
                };
                format!(
                    "INSERT INTO {table} ({}){overriding} VALUES (",
                    list.join(", ")
                )
            }
            _ => String::new(),
        };

        let params: [&(dyn ToSql + Sync); 0] = [];
        let rows = self.client.query_raw(&query, params).await?;
        let mut rows = std::pin::pin!(rows);
        let mut count = 0u64;
        while let Some(row) = rows.try_next().await? {
            let values = (0..row.len())
                .map(|i| row.try_get::<_, Option<&str>>(i))
                .collect::<Result<Vec<_>, _>>()?;
            match format {
                Format::Csv { delimiter, .. } => {
                    csv_record(
                        &mut buffer,
                        values.iter().map(|v| v.map(str::as_bytes)),
                        delimiter.byte(),
                    );
                }
                Format::Json => {
                    buffer.extend_from_slice(if count == 0 { b"\n" } else { b",\n" });
                    buffer.extend_from_slice(values[0].unwrap_or("null").as_bytes());
                }
                Format::Insert { .. } => {
                    buffer.extend_from_slice(insert_into.as_bytes());
                    for (i, value) in values.iter().enumerate() {
                        if i > 0 {
                            buffer.extend_from_slice(b", ");
                        }
                        sql_literal(&mut buffer, *value);
                    }
                    buffer.extend_from_slice(b");\n");
                }
            }
            count += 1;
            if buffer.len() >= CHUNK {
                out.write(&mut buffer).await?;
            }
            if count.is_multiple_of(PROGRESS_ROWS) {
                progress(count);
            }
        }
        if let Format::Json = format {
            buffer.extend_from_slice(if count == 0 { b"]\n" } else { b"\n]\n" });
        }
        out.write(&mut buffer).await?;
        out.writer.flush().await.map_err(file_error(path))?;
        progress(count);
        Ok(Transferred {
            rows: count,
            bytes: out.bytes,
        })
    }

    /// The positions of a table's columns that can be given values, and
    /// whether one of them is an identity ALWAYS column.
    async fn insertable(&self, relation: &str, count: usize) -> Result<(Vec<usize>, bool), Error> {
        let rows = self
            .client
            .query(
                "SELECT a.attgenerated <> '', a.attidentity = 'a'
                 FROM pg_catalog.pg_attribute a
                 WHERE a.attrelid = pg_catalog.to_regclass($1) AND a.attnum > 0 AND NOT a.attisdropped
                 ORDER BY a.attnum",
                &[&relation],
            )
            .await?;
        // A view has no such columns; keep them all.
        if rows.len() != count {
            return Ok(((0..count).collect(), false));
        }
        let kept = (0..count).filter(|&i| !rows[i].get::<_, bool>(0)).collect();
        let always = rows.iter().any(|row| row.get::<_, bool>(1));
        Ok((kept, always))
    }

    /// Appends the rows of a CSV file to a table in one transaction, with COPY.
    /// `columns` says, for each column of the file in order, which table
    /// column it fills, or `None` to skip it. Reports the bytes read so far.
    pub async fn import_csv(
        &self,
        schema: &str,
        table: &str,
        path: &Path,
        options: CsvOptions,
        columns: &[Option<String>],
        mut progress: impl FnMut(u64),
    ) -> Result<Transferred, Error> {
        let targets: Vec<String> = columns.iter().flatten().map(|c| quote_ident(c)).collect();
        if targets.is_empty() {
            return Err(Error::Refused {
                reason: Refusal::NoColumns,
                message: "Choose at least one column to import.".into(),
            });
        }
        // Skipping columns means rewriting the rows; otherwise COPY reads the
        // file as it is, quoting and all.
        let keep: Option<Vec<usize>> = (targets.len() != columns.len()).then(|| {
            columns
                .iter()
                .enumerate()
                .filter_map(|(i, c)| c.as_ref().map(|_| i))
                .collect()
        });
        let delimiter = if keep.is_some() {
            Delimiter::Comma
        } else {
            options.delimiter
        };
        // The header row goes through too, so COPY's line numbers are the file's.
        let copy = format!(
            "COPY {}.{} ({}) FROM STDIN WITH (FORMAT csv, HEADER {}, DELIMITER {})",
            quote_ident(schema),
            quote_ident(table),
            targets.join(", "),
            options.header,
            delimiter.literal(),
        );

        let read = Arc::new(AtomicU64::new(0));
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<Result<Bytes, Error>>(8);
        let producer = {
            let (path, read) = (path.to_owned(), Arc::clone(&read));
            tokio::task::spawn_blocking(move || {
                if let Err(err) = produce(&path, options, keep.as_deref(), &read, &sender) {
                    let _ = sender.blocking_send(Err(err));
                }
            })
        };

        self.client.batch_execute("BEGIN").await?;
        let copied = async {
            let sink = self
                .client
                .copy_in::<_, Bytes>(&copy)
                .await
                .map_err(with_context)?;
            let mut sink = std::pin::pin!(sink);
            let mut reported = Instant::now();
            while let Some(chunk) = receiver.recv().await {
                sink.send(chunk?).await.map_err(with_context)?;
                if reported.elapsed() >= Duration::from_millis(200) {
                    progress(read.load(Ordering::Relaxed));
                    reported = Instant::now();
                }
            }
            sink.as_mut().finish().await.map_err(with_context)
        }
        .await;
        // Stops the reader if the copy failed first.
        drop(receiver);
        let _ = producer.await;
        match copied {
            Ok(rows) => {
                self.client.batch_execute("COMMIT").await?;
                let bytes = read.load(Ordering::Relaxed);
                progress(bytes);
                Ok(Transferred { rows, bytes })
            }
            Err(err) => {
                let _ = self.client.batch_execute("ROLLBACK").await;
                Err(err)
            }
        }
    }
}

/// `.name.part` beside `path`.
fn partial_path(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(".{name}.part"))
}

/// `SELECT t.c1::text, ...` for the columns at `kept`, or all of them.
fn text_select(aliases: &[String], renamed: &str, kept: Option<&[usize]>) -> String {
    let all: Vec<usize>;
    let kept = match kept {
        Some(kept) => kept,
        None => {
            all = (0..aliases.len()).collect();
            &all
        }
    };
    let list: Vec<String> = kept
        .iter()
        .map(|&i| format!("t.{}::text", aliases[i]))
        .collect();
    format!("SELECT {} FROM {renamed}", list.join(", "))
}

/// Reads the file into chunks for COPY: transcoded to UTF-8, without a byte
/// order mark, and with only the `keep` columns when some are skipped.
fn produce(
    path: &Path,
    options: CsvOptions,
    keep: Option<&[usize]>,
    read: &Arc<AtomicU64>,
    sender: &tokio::sync::mpsc::Sender<Result<Bytes, Error>>,
) -> Result<(), Error> {
    let file = File::open(path).map_err(file_error(path))?;
    let mut decoded = decode(
        Counting {
            inner: file,
            read: Arc::clone(read),
        },
        options.encoding,
    );
    let send = |chunk: Vec<u8>| sender.blocking_send(Ok(Bytes::from(chunk))).is_ok();
    match keep {
        None => loop {
            let mut chunk = vec![0; CHUNK];
            let n = decoded.read(&mut chunk).map_err(file_error(path))?;
            if n == 0 {
                return Ok(());
            }
            chunk.truncate(n);
            if !send(chunk) {
                return Ok(());
            }
        },
        Some(keep) => {
            let mut reader = csv::ReaderBuilder::new()
                .delimiter(options.delimiter.byte())
                .has_headers(false)
                .from_reader(decoded);
            let mut record = csv::ByteRecord::new();
            let mut chunk = Vec::with_capacity(CHUNK);
            while reader.read_byte_record(&mut record).map_err(csv_error)? {
                // Quoting is gone once parsed, so an empty field is NULL, as
                // COPY reads an unquoted one.
                let fields = keep
                    .iter()
                    .map(|&i| record.get(i).filter(|f| !f.is_empty()));
                csv_record(&mut chunk, fields, b',');
                if chunk.len() >= CHUNK && !send(std::mem::take(&mut chunk)) {
                    return Ok(());
                }
            }
            send(chunk);
            Ok(())
        }
    }
}

/// The first rows of a CSV file, to choose how to import it. Without a
/// delimiter, the likeliest one is used.
pub fn preview_csv(
    path: &Path,
    delimiter: Option<Delimiter>,
    encoding: Encoding,
    limit: usize,
) -> Result<Preview, Error> {
    let size = std::fs::metadata(path).map_err(file_error(path))?.len();
    let mut start = Vec::new();
    File::open(path)
        .map_err(file_error(path))?
        .take(CHUNK as u64)
        .read_to_end(&mut start)
        .map_err(file_error(path))?;
    let looks_euc_kr = looks_euc_kr(&start);

    let mut text = String::new();
    decode(&start[..], encoding)
        .read_to_string(&mut text)
        .unwrap_or_default();
    let delimiter = delimiter.unwrap_or_else(|| likeliest_delimiter(&text));

    let file = File::open(path).map_err(file_error(path))?;
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter.byte())
        .has_headers(false)
        .flexible(true)
        .from_reader(decode(file, encoding));
    let mut rows = Vec::new();
    let mut record = csv::ByteRecord::new();
    while rows.len() < limit && reader.read_byte_record(&mut record).map_err(csv_error)? {
        rows.push(
            record
                .iter()
                .map(|field| String::from_utf8_lossy(field).into_owned())
                .collect(),
        );
    }
    Ok(Preview {
        delimiter,
        rows,
        looks_euc_kr,
        size,
    })
}

/// Reads `input` as UTF-8 (passing bytes through unchanged, so COPY rejects
/// invalid ones) or transcodes EUC-KR, dropping a byte order mark either way.
fn decode<R: Read>(input: R, encoding: Encoding) -> encoding_rs_io::DecodeReaderBytes<R, Vec<u8>> {
    let mut builder = DecodeReaderBytesBuilder::new();
    builder.strip_bom(true).bom_override(true);
    match encoding {
        // Passing through only happens without an explicit encoding; a UTF-16
        // byte order mark still gets the file transcoded.
        Encoding::Utf8 => builder.utf8_passthru(true),
        Encoding::EucKr => builder.encoding(Some(EUC_KR)),
    };
    builder.build(input)
}

/// Not UTF-8, but EUC-KR without errors: a CSV saved by Korean Excel.
fn looks_euc_kr(start: &[u8]) -> bool {
    // A character cut off at the end of the sample doesn't count.
    let whole = match start.iter().rposition(|b| b.is_ascii()) {
        Some(end) => &start[..=end],
        None => start,
    };
    std::str::from_utf8(whole).is_err()
        && EUC_KR
            .decode_without_bom_handling_and_without_replacement(whole)
            .is_some()
}

/// The delimiter found most often in the first line, outside quotes.
fn likeliest_delimiter(text: &str) -> Delimiter {
    let mut counts = [0usize; 4];
    let mut quoted = false;
    for ch in text.chars() {
        match ch {
            '"' => quoted = !quoted,
            '\n' | '\r' if !quoted => break,
            _ if !quoted => {
                if let Some(i) = Delimiter::ALL.iter().position(|d| d.byte() as char == ch) {
                    counts[i] += 1;
                }
            }
            _ => {}
        }
    }
    let best = (0..4)
        .max_by_key(|&i| (counts[i], std::cmp::Reverse(i)))
        .unwrap_or(0);
    if counts[best] == 0 {
        Delimiter::Comma
    } else {
        Delimiter::ALL[best]
    }
}

/// Appends a CSV line. `None` is NULL, written as an empty field; an empty
/// value is written `""` so it stays an empty string.
fn csv_record<'a>(
    out: &mut Vec<u8>,
    fields: impl Iterator<Item = Option<&'a [u8]>>,
    delimiter: u8,
) {
    for (i, field) in fields.enumerate() {
        if i > 0 {
            out.push(delimiter);
        }
        let Some(field) = field else { continue };
        let quote = field.is_empty()
            // COPY reads \. alone on a line as the end of the data.
            || field == b"\\."
            || field
                .iter()
                .any(|&b| b == delimiter || b == b'"' || b == b'\n' || b == b'\r');
        if quote {
            out.push(b'"');
            for &b in field {
                if b == b'"' {
                    out.push(b'"');
                }
                out.push(b);
            }
            out.push(b'"');
        } else {
            out.extend_from_slice(field);
        }
    }
    out.push(b'\n');
}

/// A string literal PostgreSQL turns into any type, or NULL.
fn sql_literal(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        None => out.extend_from_slice(b"NULL"),
        Some(value) => {
            out.push(b'\'');
            for b in value.bytes() {
                if b == b'\'' {
                    out.push(b'\'');
                }
                out.push(b);
            }
            out.push(b'\'');
        }
    }
}

struct Output<'a> {
    writer: BufWriter<tokio::fs::File>,
    path: &'a Path,
    bytes: u64,
}

impl Output<'_> {
    async fn write(&mut self, buffer: &mut Vec<u8>) -> Result<(), Error> {
        self.writer
            .write_all(buffer)
            .await
            .map_err(file_error(self.path))?;
        self.bytes += buffer.len() as u64;
        buffer.clear();
        Ok(())
    }
}

/// Counts the bytes read through it, for progress.
struct Counting<R> {
    inner: R,
    read: Arc<AtomicU64>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

fn file_error(path: &Path) -> impl Fn(io::Error) -> Error + '_ {
    move |err| Error::Storage {
        message: format!("{}: {err}", path.display()),
    }
}

fn csv_error(err: csv::Error) -> Error {
    Error::Storage {
        message: err.to_string(),
    }
}

/// A COPY error with where it happened, such as "COPY users, line 3, column
/// age: "abc"", in its detail.
fn with_context(err: tokio_postgres::Error) -> Error {
    let context = err
        .as_db_error()
        .and_then(|db| db.where_())
        .map(str::to_owned);
    match (Error::from(err), context) {
        (
            Error::Database {
                message,
                code,
                detail,
                hint,
                position,
            },
            Some(context),
        ) => Error::Database {
            message,
            code,
            detail: Some(match detail {
                Some(detail) => format!("{detail}\n{context}"),
                None => context,
            }),
            hint,
            position,
        },
        (err, _) => err,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn csv(fields: &[Option<&str>]) -> String {
        let mut out = Vec::new();
        csv_record(&mut out, fields.iter().map(|f| f.map(str::as_bytes)), b',');
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn writes_csv_as_copy_reads_it() {
        assert_eq!(csv(&[Some("a"), None, Some("")]), "a,,\"\"\n");
        assert_eq!(
            csv(&[Some("x,y"), Some("say \"hi\"")]),
            "\"x,y\",\"say \"\"hi\"\"\"\n"
        );
        assert_eq!(
            csv(&[Some("two\nlines"), Some("\\.")]),
            "\"two\nlines\",\"\\.\"\n"
        );
    }

    #[test]
    fn writes_sql_literals() {
        let mut out = Vec::new();
        sql_literal(&mut out, Some("it's"));
        out.push(b' ');
        sql_literal(&mut out, None);
        assert_eq!(String::from_utf8(out).unwrap(), "'it''s' NULL");
    }

    #[test]
    fn guesses_the_delimiter_from_the_first_line() {
        assert_eq!(likeliest_delimiter("a,b,c\n1;2"), Delimiter::Comma);
        assert_eq!(likeliest_delimiter("a\tb\tc"), Delimiter::Tab);
        assert_eq!(likeliest_delimiter("\"a,b\";c;d"), Delimiter::Semicolon);
        assert_eq!(likeliest_delimiter("single"), Delimiter::Comma);
    }

    #[test]
    fn tells_euc_kr_from_utf8() {
        let (euc_kr, _, _) = EUC_KR.encode("이름,나이\n홍길동,30\n");
        assert!(looks_euc_kr(&euc_kr));
        assert!(!looks_euc_kr("이름,나이\n".as_bytes()));
        assert!(!looks_euc_kr(b"plain,ascii\n"));
    }
}
