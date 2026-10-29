//! Replies as rows, shaped by the command that asked for them.

use std::fmt::Write as _;

use ::redis::{ServerError, Value};

use crate::postgres::StatementResult;

/// How a command's reply reads as a table.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Plain,
    /// Two columns with these names, from a map or from a flat array of pairs
    /// (RESP2's way of sending maps, and sorted set members with their scores).
    Pairs(&'static str, &'static str),
    /// Stream entries: an ID and their fields.
    Stream,
}

impl Shape {
    fn of(args: &[Vec<u8>]) -> Shape {
        let name = String::from_utf8_lossy(&args[0]).to_ascii_lowercase();
        let has = |word: &str| {
            args[1..]
                .iter()
                .any(|arg| arg.eq_ignore_ascii_case(word.as_bytes()))
        };
        match name.as_str() {
            "hgetall" => Shape::Pairs("field", "value"),
            "config"
                if args
                    .get(1)
                    .is_some_and(|sub| sub.eq_ignore_ascii_case(b"get")) =>
            {
                Shape::Pairs("field", "value")
            }
            "hrandfield" if has("withvalues") => Shape::Pairs("field", "value"),
            "zpopmin" | "zpopmax" => Shape::Pairs("member", "score"),
            name if name.starts_with('z') && has("withscores") => Shape::Pairs("member", "score"),
            "xrange" | "xrevrange" => Shape::Stream,
            _ => Shape::Plain,
        }
    }
}

/// The rows of a reply.
pub(super) fn statement_result(reply: Value, args: &[Vec<u8>], max_rows: usize) -> StatementResult {
    let (columns, total_rows, rows) = table(reply, Shape::of(args), max_rows);
    StatementResult::Rows {
        columns,
        rows,
        total_rows: total_rows as u64,
        source: None,
    }
}

type Table = (Vec<String>, usize, Vec<Vec<Option<String>>>);

fn columns(names: &[&str]) -> Vec<String> {
    names.iter().map(|&name| name.to_owned()).collect()
}

fn table(reply: Value, shape: Shape, max_rows: usize) -> Table {
    match reply {
        Value::Attribute { data, .. } => table(*data, shape, max_rows),
        Value::Map(entries) => {
            let (key, value) = match shape {
                Shape::Pairs(key, value) => (key, value),
                _ => ("field", "value"),
            };
            let total = entries.len();
            let rows = entries
                .into_iter()
                .take(max_rows)
                .map(|(k, v)| vec![cell(k), cell(v)])
                .collect();
            (columns(&[key, value]), total, rows)
        }
        Value::Array(items) | Value::Set(items) | Value::Push { data: items, .. } => {
            list(items, shape, max_rows)
        }
        scalar => {
            let rows = if max_rows > 0 {
                vec![vec![cell(scalar)]]
            } else {
                Vec::new()
            };
            (columns(&["value"]), 1, rows)
        }
    }
}

fn list(items: Vec<Value>, shape: Shape, max_rows: usize) -> Table {
    let pair = |item: &Value| matches!(item, Value::Array(pair) if pair.len() == 2 && pair.iter().all(is_scalar));
    let total = items.len();

    if let Shape::Pairs(key, value) = shape {
        // RESP3 sends sorted set members with their scores as [member, score] pairs.
        if items.iter().all(pair) {
            let rows = items
                .into_iter()
                .take(max_rows)
                .map(|item| match item {
                    Value::Array(pair) => pair.into_iter().map(cell).collect(),
                    _ => unreachable!("checked to be pairs"),
                })
                .collect();
            return (columns(&[key, value]), total, rows);
        }
        // RESP2 sends maps and pairs as one flat array.
        if total.is_multiple_of(2) && items.iter().all(is_scalar) {
            let mut items = items.into_iter();
            let mut rows = Vec::new();
            while rows.len() < max_rows {
                let (Some(k), Some(v)) = (items.next(), items.next()) else {
                    break;
                };
                rows.push(vec![cell(k), cell(v)]);
            }
            return (columns(&[key, value]), total / 2, rows);
        }
    }

    if shape == Shape::Stream
        && items
            .iter()
            .all(|item| matches!(item, Value::Array(entry) if entry.len() == 2))
    {
        let rows = items
            .into_iter()
            .take(max_rows)
            .map(|item| match item {
                Value::Array(entry) => {
                    let mut entry = entry.into_iter();
                    let id = entry.next().and_then(cell);
                    let fields = entry.next().map(|fields| fields_json(&fields));
                    vec![id, fields]
                }
                _ => unreachable!("checked to be entries"),
            })
            .collect();
        return (columns(&["id", "fields"]), total, rows);
    }

    // An array of arrays, such as SLOWLOG GET: a row per array, a column
    // per position. Nil stands for a missing row, as in GEOPOS.
    let nested = |item: &Value| matches!(item, Value::Array(_) | Value::Set(_));
    if items.iter().any(nested) && items.iter().all(|item| nested(item) || *item == Value::Nil) {
        let width = items
            .iter()
            .map(|item| match item {
                Value::Array(row) | Value::Set(row) => row.len(),
                _ => 0,
            })
            .max()
            .unwrap_or(0);
        let mut names = vec!["#".to_owned()];
        names.extend((1..=width).map(|i| i.to_string()));
        let rows = items
            .into_iter()
            .take(max_rows)
            .enumerate()
            .map(|(i, item)| {
                let mut row = vec![Some((i + 1).to_string())];
                if let Value::Array(values) | Value::Set(values) = item {
                    row.extend(values.into_iter().map(cell));
                }
                row.resize(width + 1, None);
                row
            })
            .collect();
        return (names, total, rows);
    }

    // Scalars, or a mix such as SCAN's [cursor, [keys]]: a row per item.
    let rows = items
        .into_iter()
        .take(max_rows)
        .enumerate()
        .map(|(i, item)| vec![Some((i + 1).to_string()), cell(item)])
        .collect();
    (columns(&["#", "value"]), total, rows)
}

fn is_scalar(value: &Value) -> bool {
    match value {
        Value::Array(_) | Value::Set(_) | Value::Map(_) | Value::Push { .. } => false,
        Value::Attribute { data, .. } => is_scalar(data),
        _ => true,
    }
}

/// A value as a cell: text for scalars, compact JSON for anything nested.
fn cell(value: Value) -> Option<String> {
    match value {
        Value::Nil => None,
        Value::Attribute { data, .. } => cell(*data),
        Value::Okay => Some("OK".into()),
        Value::SimpleString(text) | Value::VerbatimString { text, .. } => Some(text),
        Value::BulkString(bytes) => Some(bytes_text(&bytes)),
        Value::Int(n) => Some(n.to_string()),
        Value::Double(n) => Some(n.to_string()),
        Value::Boolean(b) => Some(b.to_string()),
        Value::BigNumber(n) => Some(n.to_string()),
        Value::ServerError(err) => Some(error_text(&err)),
        nested => {
            let mut json = String::new();
            write_json(&mut json, &nested);
            Some(json)
        }
    }
}

/// Bytes as text, or `\x` and hex when they aren't UTF-8, as PostgreSQL shows bytea.
pub(super) fn bytes_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => {
            let mut text = String::with_capacity(2 + bytes.len() * 2);
            text.push_str("\\x");
            for byte in bytes {
                let _ = write!(text, "{byte:02x}");
            }
            text
        }
    }
}

/// An error inside a reply, as redis-cli shows it.
fn error_text(err: &ServerError) -> String {
    match err.details() {
        Some(details) => format!("(error) {} {details}", err.code()),
        None => format!("(error) {}", err.code()),
    }
}

/// A stream entry's fields, `[field, value, ...]`, as a JSON object.
fn fields_json(fields: &Value) -> String {
    let mut json = String::new();
    match fields {
        Value::Array(items) if items.len().is_multiple_of(2) && items.iter().all(is_scalar) => {
            json.push('{');
            for (i, pair) in items.chunks(2).enumerate() {
                if i > 0 {
                    json.push(',');
                }
                write_key(&mut json, &pair[0]);
                json.push(':');
                write_json(&mut json, &pair[1]);
            }
            json.push('}');
        }
        other => write_json(&mut json, other),
    }
    json
}

/// Writes a value as compact JSON. Bytes that aren't UTF-8 are written as
/// in cells, `"\x..."`, which makes it JSON-ish rather than JSON.
fn write_json(json: &mut String, value: &Value) {
    match value {
        Value::Nil => json.push_str("null"),
        Value::Int(n) => {
            let _ = write!(json, "{n}");
        }
        Value::Double(n) if n.is_finite() => {
            let _ = write!(json, "{n}");
        }
        Value::Boolean(b) => {
            let _ = write!(json, "{b}");
        }
        Value::BigNumber(n) => {
            let _ = write!(json, "{n}");
        }
        Value::BulkString(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => write_string(json, text),
            Err(_) => {
                json.push('"');
                json.push_str(&bytes_text(bytes));
                json.push('"');
            }
        },
        Value::Array(items) | Value::Set(items) | Value::Push { data: items, .. } => {
            json.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    json.push(',');
                }
                write_json(json, item);
            }
            json.push(']');
        }
        Value::Map(entries) => {
            json.push('{');
            for (i, (key, value)) in entries.iter().enumerate() {
                if i > 0 {
                    json.push(',');
                }
                write_key(json, key);
                json.push(':');
                write_json(json, value);
            }
            json.push('}');
        }
        Value::Okay => write_string(json, "OK"),
        Value::SimpleString(text) | Value::VerbatimString { text, .. } => write_string(json, text),
        // inf and nan aren't JSON numbers.
        Value::Double(n) => write_string(json, &n.to_string()),
        Value::Attribute { data, .. } => write_json(json, data),
        Value::ServerError(err) => write_string(json, &error_text(err)),
        // Kinds of values the protocol may add.
        other => write_string(json, &format!("{other:?}")),
    }
}

/// Writes a map key as a JSON string, whatever its type.
fn write_key(json: &mut String, key: &Value) {
    match key {
        Value::BulkString(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => write_string(json, text),
            Err(_) => write_json(json, key),
        },
        key if is_scalar(key) => match cell(key.clone()) {
            Some(text) => write_string(json, &text),
            None => write_string(json, "null"),
        },
        key => {
            let mut text = String::new();
            write_json(&mut text, key);
            write_string(json, &text);
        }
    }
}

fn write_string(json: &mut String, text: &str) {
    json.push_str(&serde_json::Value::from(text).to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redis::command::split_arguments;

    /// Parses a reply as it comes over the wire.
    fn reply(wire: impl AsRef<[u8]>) -> Value {
        ::redis::parse_redis_value(wire.as_ref()).unwrap()
    }

    fn render(
        command: &str,
        wire: impl AsRef<[u8]>,
        max_rows: usize,
    ) -> (Vec<String>, Vec<Vec<Option<String>>>, u64) {
        let args = split_arguments(command).unwrap();
        match statement_result(reply(wire), &args, max_rows) {
            StatementResult::Rows {
                columns,
                rows,
                total_rows,
                source: None,
            } => (columns, rows, total_rows),
            other => panic!("expected rows, got {other:?}"),
        }
    }

    fn row(values: &[Option<&str>]) -> Vec<Option<String>> {
        values.iter().map(|v| v.map(str::to_owned)).collect()
    }

    #[test]
    fn renders_scalars_as_one_value() {
        let value = |wire: &[u8]| render("GET k", wire, 10);
        assert_eq!(
            value(b"$-1\r\n"),
            (columns(&["value"]), vec![row(&[None])], 1)
        );
        assert_eq!(value(b"_\r\n").1, [row(&[None])]);
        assert_eq!(value(b"+OK\r\n").1, [row(&[Some("OK")])]);
        assert_eq!(value(b"$5\r\nhello\r\n").1, [row(&[Some("hello")])]);
        assert_eq!(value(b":42\r\n").1, [row(&[Some("42")])]);
        assert_eq!(value(b",1.5\r\n").1, [row(&[Some("1.5")])]);
        assert_eq!(value(b"#t\r\n").1, [row(&[Some("true")])]);
        assert_eq!(
            value(b"(3492890328409238509324850943850943825024385\r\n").1,
            [row(&[Some("3492890328409238509324850943850943825024385")])]
        );
        assert_eq!(value(b"=7\r\ntxt:abc\r\n").1, [row(&[Some("abc")])]);
        assert_eq!(value(b"$2\r\n\xff\x00\r\n").1, [row(&[Some("\\xff00")])]);
        assert_eq!(value(b"$0\r\n\r\n").1, [row(&[Some("")])]);
    }

    #[test]
    fn renders_arrays_with_an_index() {
        let (columns, rows, total) = render("LRANGE l 0 -1", "*3\r\n$1\r\na\r\n_\r\n:3\r\n", 2);
        assert_eq!(columns, ["#", "value"]);
        assert_eq!(
            rows,
            [row(&[Some("1"), Some("a")]), row(&[Some("2"), None])]
        );
        assert_eq!(total, 3);

        let (columns, rows, _) = render("SMEMBERS s", "~1\r\n$1\r\nx\r\n", 10);
        assert_eq!(columns, ["#", "value"]);
        assert_eq!(rows, [row(&[Some("1"), Some("x")])]);

        assert_eq!(render("LRANGE l 0 -1", "*0\r\n", 10).2, 0);
    }

    #[test]
    fn renders_maps_and_pairs_as_two_columns() {
        // RESP3 map.
        let (columns, rows, total) = render(
            "HGETALL h",
            "%2\r\n$1\r\na\r\n$1\r\n1\r\n$1\r\nb\r\n$1\r\n2\r\n",
            1,
        );
        assert_eq!(columns, ["field", "value"]);
        assert_eq!(rows, [row(&[Some("a"), Some("1")])]);
        assert_eq!(total, 2);
        // RESP2 flat array.
        let (columns, rows, total) = render(
            "hgetall h",
            "*4\r\n$1\r\na\r\n$1\r\n1\r\n$1\r\nb\r\n$1\r\n2\r\n",
            10,
        );
        assert_eq!(columns, ["field", "value"]);
        assert_eq!(
            rows,
            [row(&[Some("a"), Some("1")]), row(&[Some("b"), Some("2")])]
        );
        assert_eq!(total, 2);
        assert_eq!(
            render(
                "CONFIG GET maxmemory",
                "*2\r\n$9\r\nmaxmemory\r\n$1\r\n0\r\n",
                10
            )
            .0,
            ["field", "value"]
        );
        assert_eq!(
            render(
                "HRANDFIELD h 2 WITHVALUES",
                "*2\r\n*2\r\n$1\r\na\r\n$1\r\n1\r\n*2\r\n$1\r\nb\r\n$1\r\n2\r\n",
                10
            )
            .1
            .len(),
            2
        );
        // RESP3 sorted set pairs, and RESP2's flat ones.
        let (columns, rows, _) = render(
            "ZRANGE z 0 -1 withscores",
            "*2\r\n*2\r\n$1\r\na\r\n,1\r\n*2\r\n$1\r\nb\r\n,2.5\r\n",
            10,
        );
        assert_eq!(columns, ["member", "score"]);
        assert_eq!(
            rows,
            [row(&[Some("a"), Some("1")]), row(&[Some("b"), Some("2.5")])]
        );
        let (columns, rows, _) = render("ZPOPMIN z", "*2\r\n$1\r\na\r\n$1\r\n1\r\n", 10);
        assert_eq!(columns, ["member", "score"]);
        assert_eq!(rows, [row(&[Some("a"), Some("1")])]);
        // Without WITHSCORES, members are a plain list.
        assert_eq!(
            render("ZRANGE z 0 -1", "*1\r\n$1\r\na\r\n", 10).0,
            ["#", "value"]
        );
        // Any other map is fields and values.
        assert_eq!(
            render("XINFO STREAM s", "%1\r\n$6\r\nlength\r\n:1\r\n", 10).0,
            ["field", "value"]
        );
    }

    #[test]
    fn renders_nested_replies_a_row_per_item() {
        // XRANGE: an ID and fields.
        let (columns, rows, _) = render(
            "XRANGE s - +",
            "*1\r\n*2\r\n$3\r\n1-0\r\n*4\r\n$4\r\nname\r\n$3\r\nann\r\n$3\r\nage\r\n$1\r\n3\r\n",
            10,
        );
        assert_eq!(columns, ["id", "fields"]);
        assert_eq!(
            rows,
            [row(&[Some("1-0"), Some(r#"{"name":"ann","age":"3"}"#)])]
        );

        // SCAN: the cursor, then the keys.
        let (columns, rows, _) = render(
            "SCAN 0",
            b"*2\r\n$1\r\n0\r\n*2\r\n$1\r\na\r\n$2\r\n\xff\x01\r\n",
            10,
        );
        assert_eq!(columns, ["#", "value"]);
        assert_eq!(
            rows,
            [
                row(&[Some("1"), Some("0")]),
                row(&[Some("2"), Some(r#"["a","\xff01"]"#)])
            ]
        );

        // Arrays of arrays: a column per position, nil for a missing row.
        let (columns, rows, _) = render(
            "GEOPOS g a b",
            "*2\r\n*2\r\n$3\r\n1.5\r\n$3\r\n2.5\r\n_\r\n",
            10,
        );
        assert_eq!(columns, ["#", "1", "2"]);
        assert_eq!(
            rows,
            [
                row(&[Some("1"), Some("1.5"), Some("2.5")]),
                row(&[Some("2"), None, None])
            ]
        );

        // Deeper values are compact JSON.
        let (_, rows, _) = render(
            "SLOWLOG GET",
            "*1\r\n*3\r\n:1\r\n*2\r\n$3\r\nGET\r\n$1\r\nk\r\n%1\r\n$1\r\na\r\n#f\r\n",
            10,
        );
        assert_eq!(
            rows,
            [row(&[
                Some("1"),
                Some("1"),
                Some(r#"["GET","k"]"#),
                Some(r#"{"a":false}"#)
            ])]
        );
    }

    #[test]
    fn renders_nested_errors_as_text() {
        let (_, rows, _) = render("EVAL x 0", "*2\r\n:1\r\n-ERR boom\r\n", 10);
        assert_eq!(rows[1], row(&[Some("2"), Some("(error) ERR boom")]));
    }
}
