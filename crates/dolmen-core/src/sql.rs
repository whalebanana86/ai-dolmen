//! SQL text utilities.

use std::ops::Range;

use serde::Serialize;

use crate::connections::Engine;

/// Splits PostgreSQL SQL text into statements.
///
/// Returns the byte range of each statement, from its first token to its
/// terminating `;` (inclusive), or to its last token when the `;` is missing.
/// Semicolons inside strings, quoted identifiers, dollar-quoted bodies and
/// comments don't end a statement. Comments before a statement are not part of
/// it, and text with only whitespace and comments has no statements.
///
/// Not handled yet: SQL-standard function bodies (`BEGIN ATOMIC ... END`),
/// which contain bare semicolons.
pub fn split_statements(sql: &str) -> Vec<Range<usize>> {
    // Scanning bytes is safe for UTF-8: every byte the lexer looks for is ASCII,
    // and multi-byte characters never contain ASCII bytes.
    let bytes = sql.as_bytes();
    let mut statements = Vec::new();
    let mut start = None;
    let mut end = 0;
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i = line_comment_end(bytes, i);
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = block_comment_end(bytes, i);
                continue;
            }
            b';' => {
                if let Some(start) = start.take() {
                    statements.push(start..i + 1);
                }
                i += 1;
                continue;
            }
            byte if byte.is_ascii_whitespace() => {
                i += 1;
                continue;
            }
            _ => {}
        }

        start.get_or_insert(i);
        i = match bytes[i] {
            b'\'' => quoted_end(bytes, i, b'\'', is_escape_string(bytes, i)),
            b'"' => quoted_end(bytes, i, b'"', false),
            b'$' => dollar_quoted_end(bytes, i).unwrap_or(i + 1),
            _ => i + 1,
        };
        end = i;
    }

    if let Some(start) = start {
        statements.push(start..end);
    }
    statements
}

/// Finds the statement to run for a cursor at byte `offset`, like DataGrip does.
///
/// A cursor inside a statement, or right after it, picks that statement. Between
/// statements, the previous one wins while the cursor is still on its last line;
/// otherwise the next one does.
pub fn statement_at(sql: &str, offset: usize) -> Option<Range<usize>> {
    pick(sql, offset, split_statements(sql))
}

/// The statements in `text`, written for `engine`: SQL for the SQL
/// databases, one command a line for Redis, mongosh statements for MongoDB.
pub fn split_for(engine: Engine, text: &str) -> Vec<Range<usize>> {
    match engine {
        Engine::Postgres => split_statements(text),
        Engine::MySql | Engine::MariaDb => split_mysql_statements(text),
        Engine::Redis => split_lines(text),
        Engine::MongoDb | Engine::DocumentDb => crate::mongo::statement_ranges(text),
    }
}

/// [`statement_at`] for text written for `engine`.
pub fn statement_at_for(engine: Engine, text: &str, offset: usize) -> Option<Range<usize>> {
    pick(text, offset, split_for(engine, text))
}

/// The statement among `statements` to run for a cursor at `offset`.
fn pick(sql: &str, offset: usize, statements: Vec<Range<usize>>) -> Option<Range<usize>> {
    let offset = offset.min(sql.len());

    if let Some(statement) = statements
        .iter()
        .find(|s| s.start <= offset && offset <= s.end)
    {
        return Some(statement.clone());
    }

    let previous = statements.iter().rev().find(|s| s.end <= offset);
    let next = statements.iter().find(|s| s.start > offset);
    match (previous, next) {
        (Some(previous), _) if !sql[previous.end..offset].contains('\n') => Some(previous.clone()),
        (_, Some(next)) => Some(next.clone()),
        (previous, None) => previous.cloned(),
    }
}

/// Splits MySQL or MariaDB SQL text into statements, as the `mysql` client
/// does: `#` and `-- ` comments, backslash escapes in strings, backquoted
/// names, and `DELIMITER` lines, which change what ends a statement so a
/// procedure's body can hold `;`. A custom delimiter is left out of the
/// statement's range, as the server doesn't know it; `;` is kept.
pub fn split_mysql_statements(sql: &str) -> Vec<Range<usize>> {
    let bytes = sql.as_bytes();
    let mut delimiter: &[u8] = b";";
    let mut statements = Vec::new();
    let mut start = None;
    let mut end = 0;
    let mut i = 0;

    while i < bytes.len() {
        if start.is_none() && at_line_start(bytes, i) && is_delimiter_command(&bytes[i..]) {
            let line_end = bytes[i..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(bytes.len(), |n| i + n);
            let word = bytes[i + "delimiter".len()..line_end]
                .split(u8::is_ascii_whitespace)
                .find(|word| !word.is_empty());
            if let Some(word) = word {
                delimiter = word;
            }
            i = line_end;
            continue;
        }
        match bytes[i] {
            b'-' if bytes.get(i + 1) == Some(&b'-')
                && bytes.get(i + 2).is_none_or(u8::is_ascii_whitespace) =>
            {
                i = line_comment_end(bytes, i);
                continue;
            }
            b'#' => {
                i = line_comment_end(bytes, i);
                continue;
            }
            // `/*! ... */` is code for MySQL, so it belongs to the statement.
            b'/' if bytes.get(i + 1) == Some(&b'*') && bytes.get(i + 2) != Some(&b'!') => {
                i = flat_comment_end(bytes, i);
                continue;
            }
            _ if bytes[i..].starts_with(delimiter) => {
                if let Some(start) = start.take() {
                    let kept = if delimiter == b";" { i + 1 } else { end };
                    statements.push(start..kept);
                }
                i += delimiter.len();
                continue;
            }
            byte if byte.is_ascii_whitespace() => {
                i += 1;
                continue;
            }
            _ => {}
        }

        start.get_or_insert(i);
        i = match bytes[i] {
            b'\'' => quoted_end(bytes, i, b'\'', true),
            b'"' => quoted_end(bytes, i, b'"', true),
            b'`' => quoted_end(bytes, i, b'`', false),
            b'/' if bytes.get(i + 1) == Some(&b'*') => flat_comment_end(bytes, i),
            _ => i + 1,
        };
        end = i;
    }

    if let Some(start) = start {
        statements.push(start..end);
    }
    statements
}

/// Whether `rest` starts with the `mysql` client's `DELIMITER` command.
fn is_delimiter_command(rest: &[u8]) -> bool {
    let word = b"delimiter";
    rest.len() > word.len()
        && rest[..word.len()].eq_ignore_ascii_case(word)
        && rest[word.len()].is_ascii_whitespace()
}

/// Whether only spaces and tabs come before `i` on its line.
fn at_line_start(bytes: &[u8], i: usize) -> bool {
    bytes[..i]
        .iter()
        .rev()
        .take_while(|&&b| b != b'\n')
        .all(|&b| b == b' ' || b == b'\t' || b == b'\r')
}

/// End of a `/* ... */` comment that doesn't nest, as in MySQL.
fn flat_comment_end(bytes: &[u8], start: usize) -> usize {
    bytes[start + 2..]
        .windows(2)
        .position(|pair| pair == b"*/")
        .map_or(bytes.len(), |close| start + 2 + close + 2)
}

/// One Redis command a line, as `redis-cli` reads them: blank lines and
/// lines starting with `#` aren't commands.
fn split_lines(text: &str) -> Vec<Range<usize>> {
    let mut commands = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            let start = offset + (line.len() - line.trim_start().len());
            commands.push(start..start + trimmed.len());
        }
        offset += line.len();
    }
    commands
}

/// What a statement can destroy, to ask before it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Risk {
    /// An UPDATE or DELETE without a WHERE clause, which touches every row.
    EveryRow { verb: String, table: String },
    /// A DROP, such as `TABLE IF EXISTS public.orders`; `table` when it drops one.
    Drop {
        object: String,
        table: Option<String>,
    },
    /// TRUNCATE, which empties the table.
    Truncate { table: String },
    /// ALTER TABLE ... DROP COLUMN, which loses the column's data.
    DropColumn { table: String, column: String },
}

/// What `statement` (one statement) can destroy, if anything. Only plain
/// UPDATE, DELETE, TRUNCATE, DROP and ALTER TABLE ... DROP are recognized;
/// a WHERE inside parentheses, as in a subquery, doesn't count as the
/// statement's own.
pub fn risk(statement: &str) -> Option<Risk> {
    let tokens = tokens(statement);
    let top: Vec<&str> = tokens
        .iter()
        .filter(|t| t.depth == 0)
        .map(|t| t.text)
        .collect();
    let word = |i: usize| {
        top.get(i)
            .map(|t| t.to_ascii_uppercase())
            .unwrap_or_default()
    };
    let has_where = top.iter().any(|t| t.eq_ignore_ascii_case("where"));
    let skip = |mut i: usize, words: &[&str]| {
        while words.contains(&word(i).as_str()) {
            i += 1;
        }
        i
    };
    match word(0).as_str() {
        "UPDATE" => {
            let (table, _) = name(&top, skip(1, &["ONLY"]))?;
            (!has_where).then(|| Risk::EveryRow {
                verb: "UPDATE".into(),
                table,
            })
        }
        "DELETE" if word(1) == "FROM" => {
            let (table, _) = name(&top, skip(2, &["ONLY"]))?;
            (!has_where).then(|| Risk::EveryRow {
                verb: "DELETE".into(),
                table,
            })
        }
        "TRUNCATE" => {
            let (table, _) = name(&top, skip(1, &["TABLE", "ONLY"]))?;
            Some(Risk::Truncate { table })
        }
        "DROP" => {
            let object = statement
                .trim()
                .trim_end_matches(';')
                .split_whitespace()
                .skip(1)
                .collect::<Vec<_>>()
                .join(" ");
            let table = (word(1) == "TABLE")
                .then(|| name(&top, skip(2, &["IF", "EXISTS"])).map(|(n, _)| n))
                .flatten();
            Some(Risk::Drop { object, table })
        }
        "ALTER" if word(1) == "TABLE" => {
            let (table, mut i) = name(&top, skip(2, &["IF", "EXISTS", "ONLY"]))?;
            while i < top.len() {
                if word(i) == "DROP" {
                    match word(i + 1).as_str() {
                        // Dropping a default, NOT NULL or a constraint keeps the data.
                        "CONSTRAINT" | "DEFAULT" | "NOT" | "IDENTITY" | "EXPRESSION" => {}
                        "COLUMN" => {
                            let (column, _) = name(&top, skip(i + 2, &["IF", "EXISTS"]))?;
                            return Some(Risk::DropColumn { table, column });
                        }
                        _ => {
                            let (column, _) = name(&top, skip(i + 1, &["IF", "EXISTS"]))?;
                            return Some(Risk::DropColumn { table, column });
                        }
                    }
                }
                i += 1;
            }
            None
        }
        _ => None,
    }
}

/// A possibly qualified name starting at `top[i]`, as written, and the index after it.
fn name(top: &[&str], i: usize) -> Option<(String, usize)> {
    let is_part =
        |t: &&str| t.starts_with('"') || t.starts_with(|c: char| c.is_alphanumeric() || c == '_');
    let first = top.get(i).filter(|t| is_part(t))?;
    let mut name = (*first).to_owned();
    let mut i = i + 1;
    while top.get(i) == Some(&".") && top.get(i + 1).is_some_and(is_part) {
        name.push('.');
        name.push_str(top[i + 1]);
        i += 2;
    }
    Some((name, i))
}

struct Token<'a> {
    text: &'a str,
    /// How deep in parentheses.
    depth: u32,
}

/// The statement up to its last token: without the semicolon, comments and
/// space after it, so it can be wrapped in more SQL.
pub fn without_terminator(statement: &str) -> &str {
    let bytes = statement.as_bytes();
    let mut end = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i = line_comment_end(bytes, i);
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = block_comment_end(bytes, i);
                continue;
            }
            byte if byte.is_ascii_whitespace() || byte == b';' => {
                i += 1;
                continue;
            }
            b'\'' => i = quoted_end(bytes, i, b'\'', is_escape_string(bytes, i)),
            b'"' => i = quoted_end(bytes, i, b'"', false),
            b'$' => i = dollar_quoted_end(bytes, i).unwrap_or(i + 1),
            _ => i += 1,
        }
        end = i;
    }
    &statement[..end]
}

/// Words, quoted names and punctuation, with comments and string contents
/// left out (a string is one `''` token).
fn tokens(sql: &str) -> Vec<Token<'_>> {
    let bytes = sql.as_bytes();
    let mut tokens = Vec::new();
    let mut depth = 0u32;
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        match bytes[i] {
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i = line_comment_end(bytes, i);
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = block_comment_end(bytes, i);
                continue;
            }
            byte if byte.is_ascii_whitespace() => {
                i += 1;
                continue;
            }
            b'\'' => {
                i = quoted_end(bytes, i, b'\'', is_escape_string(bytes, i));
                tokens.push(Token { text: "''", depth });
                continue;
            }
            b'"' => i = quoted_end(bytes, i, b'"', false),
            b'$' => match dollar_quoted_end(bytes, i) {
                Some(end) => {
                    i = end;
                    tokens.push(Token { text: "''", depth });
                    continue;
                }
                None => i += 1,
            },
            b'(' => {
                tokens.push(Token { text: "(", depth });
                depth += 1;
                i += 1;
                continue;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                tokens.push(Token { text: ")", depth });
                i += 1;
                continue;
            }
            byte if is_identifier_byte(byte) || !byte.is_ascii() => {
                while i < bytes.len() && (is_identifier_byte(bytes[i]) || !bytes[i].is_ascii()) {
                    i += 1;
                }
            }
            _ => i += 1,
        }
        tokens.push(Token {
            text: &sql[start..i],
            depth,
        });
    }
    tokens
}

fn line_comment_end(bytes: &[u8], start: usize) -> usize {
    bytes[start..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(bytes.len(), |newline| start + newline + 1)
}

/// PostgreSQL block comments nest: `/* a /* b */ c */` is one comment.
fn block_comment_end(bytes: &[u8], start: usize) -> usize {
    let mut depth = 0;
    let mut i = start;
    while i + 1 < bytes.len() {
        match (bytes[i], bytes[i + 1]) {
            (b'/', b'*') => {
                depth += 1;
                i += 2;
            }
            (b'*', b'/') => {
                depth -= 1;
                i += 2;
                if depth == 0 {
                    return i;
                }
            }
            _ => i += 1,
        }
    }
    bytes.len()
}

/// End of a `'string'` or `"identifier"` starting at `start`. A doubled quote
/// is an escaped quote; escape strings (`E'...'`) also allow backslash escapes.
fn quoted_end(bytes: &[u8], start: usize, quote: u8, backslash_escapes: bool) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if backslash_escapes => i += 2,
            byte if byte == quote => {
                if bytes.get(i + 1) == Some(&quote) {
                    i += 2;
                } else {
                    return i + 1;
                }
            }
            _ => i += 1,
        }
    }
    bytes.len()
}

/// Whether the quote at `quote` opens an escape string such as `E'\n'`.
fn is_escape_string(bytes: &[u8], quote: usize) -> bool {
    quote > 0
        && matches!(bytes[quote - 1], b'e' | b'E')
        && (quote < 2 || !is_identifier_byte(bytes[quote - 2]))
}

/// End of a dollar-quoted string such as `$$...$$` or `$fn$...$fn$` starting at
/// `start`, or `None` if the `$` doesn't open one (e.g. `$1` or `a$b`).
fn dollar_quoted_end(bytes: &[u8], start: usize) -> Option<usize> {
    if start > 0 && is_identifier_byte(bytes[start - 1]) {
        return None;
    }
    let tag_len = bytes[start + 1..]
        .iter()
        .position(|&b| !is_identifier_byte(b) || b == b'$')
        .unwrap_or(bytes.len() - start - 1);
    let delimiter_end = start + 1 + tag_len;
    if bytes.get(delimiter_end) != Some(&b'$')
        || bytes.get(start + 1).is_some_and(u8::is_ascii_digit)
    {
        return None;
    }

    let delimiter = &bytes[start..=delimiter_end];
    let body = &bytes[delimiter_end + 1..];
    Some(
        body.windows(delimiter.len())
            .position(|window| window == delimiter)
            .map_or(bytes.len(), |close| {
                delimiter_end + 1 + close + delimiter.len()
            }),
    )
}

/// Bytes that can appear inside an unquoted identifier, including UTF-8 bytes
/// of non-ASCII letters.
fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || byte >= 0x80
}

#[cfg(test)]
mod tests {
    #[test]
    fn splits_mysql_text_like_the_mysql_client() {
        use super::split_mysql_statements;
        let text = |sql: &str| -> Vec<String> {
            split_mysql_statements(sql)
                .into_iter()
                .map(|range| sql[range].to_owned())
                .collect()
        };
        assert_eq!(
            text("SELECT 'a;\\'b'; # a ; comment\nSELECT `x;y` / 2 -- c;\n;SELECT \"q\\\";\";"),
            [
                "SELECT 'a;\\'b';",
                "SELECT `x;y` / 2 -- c;\n;",
                "SELECT \"q\\\";\";"
            ]
        );
        // `--` without a space after it isn't a comment in MySQL.
        assert_eq!(text("SELECT 1--1;"), ["SELECT 1--1;"]);
        assert_eq!(
            text(
                "DELIMITER $$\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END$$\nDELIMITER ;\nCALL p();"
            ),
            [
                "CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END",
                "CALL p();"
            ]
        );
        assert_eq!(
            text("/*!40101 SET NAMES utf8 */; /* plain; */ SELECT 1"),
            ["/*!40101 SET NAMES utf8 */;", "SELECT 1"]
        );
    }

    #[test]
    fn splits_redis_commands_by_line() {
        use super::split_for;
        use crate::connections::Engine;
        let text = "SET a 1\n\n  # note\n  GET \"a b\"  \r\nDEL a";
        let commands: Vec<&str> = split_for(Engine::Redis, text)
            .into_iter()
            .map(|range| &text[range])
            .collect();
        assert_eq!(commands, ["SET a 1", "GET \"a b\"", "DEL a"]);
    }

    #[test]
    fn strips_what_follows_the_last_token() {
        assert_eq!(without_terminator("SELECT 1; -- done\n"), "SELECT 1");
        assert_eq!(without_terminator("SELECT 1 -- note\n;"), "SELECT 1");
        assert_eq!(without_terminator("SELECT ';' /* c */ ;;"), "SELECT ';'");
        assert_eq!(without_terminator("SELECT $$a;$$"), "SELECT $$a;$$");
        assert_eq!(without_terminator("SELECT '이름'  "), "SELECT '이름'");
    }

    use super::*;

    fn split(sql: &str) -> Vec<&str> {
        split_statements(sql).into_iter().map(|r| &sql[r]).collect()
    }

    fn at(sql: &str, offset: usize) -> Option<&str> {
        statement_at(sql, offset).map(|r| &sql[r])
    }

    #[test]
    fn recognizes_statements_that_destroy_data() {
        let every = |verb: &str, table: &str| {
            Some(Risk::EveryRow {
                verb: verb.into(),
                table: table.into(),
            })
        };
        assert_eq!(risk("DELETE FROM orders"), every("DELETE", "orders"));
        assert_eq!(
            risk("delete from only public.orders;"),
            every("DELETE", "public.orders")
        );
        assert_eq!(
            risk(r#"UPDATE "Shop"."Orders" SET total = 0"#),
            every("UPDATE", r#""Shop"."Orders""#)
        );
        // A WHERE in a subquery or a string isn't the statement's own.
        assert_eq!(
            risk("UPDATE t SET a = (SELECT 1 FROM u WHERE u.id = 2)"),
            every("UPDATE", "t")
        );
        assert_eq!(
            risk("UPDATE t SET note = 'where' -- where\n"),
            every("UPDATE", "t")
        );
        assert_eq!(risk("DELETE FROM t WHERE id = 1"), None);
        assert_eq!(risk("UPDATE t SET a = 1 WHERE CURRENT OF c"), None);
        assert_eq!(risk("SELECT * FROM t"), None);
        assert_eq!(risk("INSERT INTO t VALUES (1)"), None);

        assert_eq!(
            risk("TRUNCATE TABLE ONLY sales.items CASCADE"),
            Some(Risk::Truncate {
                table: "sales.items".into()
            })
        );
        assert_eq!(
            risk("DROP TABLE IF EXISTS public.orders CASCADE;"),
            Some(Risk::Drop {
                object: "TABLE IF EXISTS public.orders CASCADE".into(),
                table: Some("public.orders".into())
            })
        );
        assert_eq!(
            risk("drop schema shop"),
            Some(Risk::Drop {
                object: "schema shop".into(),
                table: None
            })
        );
        assert_eq!(
            risk("ALTER TABLE users DROP COLUMN IF EXISTS email"),
            Some(Risk::DropColumn {
                table: "users".into(),
                column: "email".into()
            })
        );
        assert_eq!(
            risk("ALTER TABLE users ADD COLUMN a int, DROP b"),
            Some(Risk::DropColumn {
                table: "users".into(),
                column: "b".into()
            })
        );
        assert_eq!(risk("ALTER TABLE users ALTER COLUMN a DROP DEFAULT"), None);
        assert_eq!(risk("ALTER TABLE users ALTER COLUMN a DROP NOT NULL"), None);
        assert_eq!(risk("ALTER TABLE users DROP CONSTRAINT users_pkey"), None);
    }

    #[test]
    fn splits_on_semicolons() {
        assert_eq!(split("SELECT 1; SELECT 2;"), ["SELECT 1;", "SELECT 2;"]);
        assert_eq!(split("SELECT 1;\nSELECT 2"), ["SELECT 1;", "SELECT 2"]);
        assert_eq!(split("  SELECT\n  1  ;"), ["SELECT\n  1  ;"]);
    }

    #[test]
    fn ignores_empty_statements_and_comment_only_text() {
        assert_eq!(split(";;SELECT 1;;"), ["SELECT 1;"]);
        assert!(split("-- nothing\n/* here */\n").is_empty());
        assert!(split("").is_empty());
    }

    #[test]
    fn leaves_leading_comments_out_but_keeps_inner_ones() {
        assert_eq!(
            split("SELECT 1; -- first\nSELECT /* two */ 2;"),
            ["SELECT 1;", "SELECT /* two */ 2;"]
        );
    }

    #[test]
    fn ignores_semicolons_in_strings_identifiers_and_comments() {
        assert_eq!(
            split("SELECT ';', \"a;b\" -- c;\nFROM t; /* ; */ SELECT 2;"),
            ["SELECT ';', \"a;b\" -- c;\nFROM t;", "SELECT 2;"]
        );
        assert_eq!(
            split("SELECT 'it''s;'; SELECT 2"),
            ["SELECT 'it''s;';", "SELECT 2"]
        );
        assert_eq!(
            split("SELECT \"a\"\";\"; SELECT 2"),
            ["SELECT \"a\"\";\";", "SELECT 2"]
        );
    }

    #[test]
    fn handles_escape_strings() {
        assert_eq!(
            split(r"SELECT E'it\'s;'; SELECT 2"),
            [r"SELECT E'it\'s;';", "SELECT 2"]
        );
        // In a plain string a backslash is literal, so the string ends after it.
        assert_eq!(
            split(r"SELECT 'a\'; SELECT 2"),
            [r"SELECT 'a\';", "SELECT 2"]
        );
        // `nE'...'`'s E belongs to an identifier, so this is a plain string.
        assert_eq!(
            split(r"SELECT typE'a\'; SELECT 2"),
            [r"SELECT typE'a\';", "SELECT 2"]
        );
    }

    #[test]
    fn handles_dollar_quoting() {
        let function = "CREATE FUNCTION f() RETURNS int AS $$ SELECT 1; $$ LANGUAGE sql;";
        assert_eq!(
            split(&format!("{function} SELECT 2")),
            [function, "SELECT 2"]
        );

        let tagged = "DO $body$ BEGIN PERFORM $$;$$; END $body$;";
        assert_eq!(
            split(&format!("{tagged}\nSELECT 2;")),
            [tagged, "SELECT 2;"]
        );
    }

    #[test]
    fn does_not_mistake_dollars_for_quotes() {
        assert_eq!(split("SELECT $1; SELECT 2"), ["SELECT $1;", "SELECT 2"]);
        assert_eq!(
            split("SELECT x$y$z; SELECT 2"),
            ["SELECT x$y$z;", "SELECT 2"]
        );
    }

    #[test]
    fn handles_nested_block_comments() {
        assert_eq!(
            split("/* a /* b; */ c; */ SELECT 1; SELECT 2"),
            ["SELECT 1;", "SELECT 2"]
        );
    }

    #[test]
    fn runs_unterminated_quotes_to_the_end() {
        assert_eq!(split("SELECT 'abc; SELECT 2"), ["SELECT 'abc; SELECT 2"]);
        assert_eq!(split("SELECT $$abc; SELECT 2"), ["SELECT $$abc; SELECT 2"]);
    }

    #[test]
    fn keeps_multibyte_text_intact() {
        assert_eq!(
            split("SELECT 'é;'; SELECT '한글'"),
            ["SELECT 'é;';", "SELECT '한글'"]
        );
        assert_eq!(split("SELECT 이름 FROM 사람;"), ["SELECT 이름 FROM 사람;"]);
    }

    const SCRIPT: &str = "SELECT 1;\nSELECT\n  2;\n\n-- third\nSELECT 3;\n";

    #[test]
    fn picks_the_statement_under_the_cursor() {
        assert_eq!(at(SCRIPT, 3), Some("SELECT 1;"));
        // Anywhere in a multi-line statement runs all of it.
        assert_eq!(at(SCRIPT, SCRIPT.find("2;").unwrap()), Some("SELECT\n  2;"));
    }

    #[test]
    fn picks_the_statement_just_before_the_cursor_on_the_same_line() {
        assert_eq!(at(SCRIPT, "SELECT 1;".len()), Some("SELECT 1;"));
        assert_eq!(at("SELECT 1;   ", 11), Some("SELECT 1;"));
    }

    #[test]
    fn picks_the_next_statement_from_blank_and_comment_lines() {
        let blank_line = SCRIPT.find("\n\n").unwrap() + 1;
        assert_eq!(at(SCRIPT, blank_line), Some("SELECT 3;"));
        assert_eq!(at(SCRIPT, SCRIPT.find("third").unwrap()), Some("SELECT 3;"));
    }

    #[test]
    fn falls_back_to_the_last_statement_at_the_end() {
        assert_eq!(at(SCRIPT, SCRIPT.len()), Some("SELECT 3;"));
        assert_eq!(at(SCRIPT, SCRIPT.len() + 10), Some("SELECT 3;"));
    }

    #[test]
    fn finds_nothing_without_statements() {
        assert_eq!(at("", 0), None);
        assert_eq!(at("-- just a comment", 5), None);
    }
}
