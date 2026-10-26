//! SQL text utilities.

use std::ops::Range;

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
    let offset = offset.min(sql.len());
    let statements = split_statements(sql);

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
    use super::*;

    fn split(sql: &str) -> Vec<&str> {
        split_statements(sql).into_iter().map(|r| &sql[r]).collect()
    }

    fn at(sql: &str, offset: usize) -> Option<&str> {
        statement_at(sql, offset).map(|r| &sql[r])
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
