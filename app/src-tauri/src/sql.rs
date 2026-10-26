//! SQL text commands for the editor.
//!
//! The editor counts positions in UTF-16 code units (JavaScript string indices)
//! while dolmen-core works with UTF-8 byte offsets, so these commands convert.

use std::ops::Range;

use serde::Serialize;

/// A range in the editor text, in UTF-16 code units.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct TextRange {
    start: usize,
    end: usize,
}

/// The statement to run for a cursor at `cursor`.
#[tauri::command]
pub fn sql_statement_at(sql: String, cursor: usize) -> Option<TextRange> {
    dolmen_core::sql::statement_at(&sql, byte_offset(&sql, cursor))
        .map(|range| utf16_range(&sql, range))
}

/// Every statement in `sql`, in order.
#[tauri::command]
pub fn sql_split(sql: String) -> Vec<TextRange> {
    dolmen_core::sql::split_statements(&sql)
        .into_iter()
        .map(|range| utf16_range(&sql, range))
        .collect()
}

fn byte_offset(text: &str, utf16_offset: usize) -> usize {
    let mut units = 0;
    for (byte, ch) in text.char_indices() {
        if units >= utf16_offset {
            return byte;
        }
        units += ch.len_utf16();
    }
    text.len()
}

fn utf16_range(text: &str, range: Range<usize>) -> TextRange {
    let start = text[..range.start].encode_utf16().count();
    TextRange {
        start,
        end: start + text[range].encode_utf16().count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // '😀' is 4 bytes in UTF-8 but 2 units in UTF-16, and '한' is 3 bytes but 1 unit.
    const SQL: &str = "SELECT '😀한';\nSELECT 2;";

    fn js_index(text: &str, needle: &str) -> usize {
        text[..text.find(needle).unwrap()].encode_utf16().count()
    }

    #[test]
    fn converts_between_utf16_and_byte_offsets() {
        let second = js_index(SQL, "SELECT 2");
        assert_eq!(
            sql_statement_at(SQL.into(), second + 3),
            Some(TextRange {
                start: second,
                end: second + "SELECT 2;".len()
            })
        );
        assert_eq!(
            sql_split(SQL.into()),
            [
                TextRange {
                    start: 0,
                    end: second - 1
                },
                TextRange {
                    start: second,
                    end: second + "SELECT 2;".len()
                },
            ]
        );
    }
}
