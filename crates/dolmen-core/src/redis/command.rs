//! The editor's text as commands, split into arguments as redis-cli does,
//! and arguments quoted back for it.

use std::fmt::Write as _;

use crate::Error;

/// A command from [`Session::execute`](super::Session::execute)'s text.
#[derive(Debug)]
pub(super) struct Line {
    pub(super) args: Vec<Vec<u8>>,
    /// 1-based character position of the command in the text, for errors.
    pub(super) position: u32,
}

/// Splits the text into commands, one per line.
pub(super) fn parse(text: &str) -> Result<Vec<Line>, Error> {
    let mut commands = Vec::new();
    let mut line_start = 0;
    for line in text.split('\n') {
        let start = line_start;
        line_start += line.chars().count() + 1;
        let command = line.trim_start();
        if command.trim_end().is_empty() || command.starts_with('#') {
            continue;
        }
        let indent = line.chars().count() - command.chars().count();
        let position = (start + indent + 1) as u32;
        let args = split_arguments(command).map_err(|(at, message)| Error::Database {
            message: message.into(),
            code: "ERR".into(),
            detail: None,
            hint: None,
            position: Some(position + command[..at].chars().count() as u32),
        })?;
        commands.push(Line { args, position });
    }
    Ok(commands)
}

/// Splits a line into arguments as redis-cli does (`sdssplitargs`). Returns
/// the byte offset of a quoting error and what's wrong.
pub(super) fn split_arguments(line: &str) -> Result<Vec<Vec<u8>>, (usize, &'static str)> {
    let bytes = line.as_bytes();
    let blank = |i: usize| bytes.get(i).is_some_and(|b| b" \t\r\n\x0b\x0c".contains(b));
    let hex = |i: usize| bytes.get(i).is_some_and(u8::is_ascii_hexdigit);
    let closing = |i: usize| {
        if i + 1 < bytes.len() && !blank(i + 1) {
            Err((i + 1, "a closing quote must be followed by a space"))
        } else {
            Ok(())
        }
    };

    let mut args = Vec::new();
    let mut i = 0;
    loop {
        while blank(i) {
            i += 1;
        }
        if i >= bytes.len() {
            return Ok(args);
        }
        let mut arg = Vec::new();
        loop {
            match bytes.get(i) {
                None => break,
                Some(_) if blank(i) => break,
                Some(b'"') => {
                    let opened = i;
                    i += 1;
                    loop {
                        match bytes.get(i) {
                            None => return Err((opened, "unbalanced quotes")),
                            Some(b'\\')
                                if bytes.get(i + 1) == Some(&b'x') && hex(i + 2) && hex(i + 3) =>
                            {
                                let digits =
                                    std::str::from_utf8(&bytes[i + 2..i + 4]).expect("ASCII");
                                arg.push(u8::from_str_radix(digits, 16).expect("hex digits"));
                                i += 4;
                            }
                            Some(b'\\') if i + 1 < bytes.len() => {
                                arg.push(match bytes[i + 1] {
                                    b'n' => b'\n',
                                    b'r' => b'\r',
                                    b't' => b'\t',
                                    b'b' => 0x08,
                                    b'a' => 0x07,
                                    other => other,
                                });
                                i += 2;
                            }
                            Some(b'"') => {
                                closing(i)?;
                                i += 1;
                                break;
                            }
                            Some(&byte) => {
                                arg.push(byte);
                                i += 1;
                            }
                        }
                    }
                }
                Some(b'\'') => {
                    let opened = i;
                    i += 1;
                    loop {
                        match bytes.get(i) {
                            None => return Err((opened, "unbalanced quotes")),
                            Some(b'\\') if bytes.get(i + 1) == Some(&b'\'') => {
                                arg.push(b'\'');
                                i += 2;
                            }
                            Some(b'\'') => {
                                closing(i)?;
                                i += 1;
                                break;
                            }
                            Some(&byte) => {
                                arg.push(byte);
                                i += 1;
                            }
                        }
                    }
                }
                Some(&byte) => {
                    arg.push(byte);
                    i += 1;
                }
            }
        }
        args.push(arg);
    }
}

/// Quotes an argument for [`Session::execute`](super::Session::execute) when it needs it, such as a
/// key name with spaces. A [`Key::name`](super::Key::name) that stands for bytes that aren't
/// UTF-8 (`\x` and hex) becomes those bytes again.
pub fn quote(argument: &str) -> String {
    if let Some(bytes) = binary_name(argument) {
        let mut quoted = String::from("\"");
        for byte in bytes {
            let _ = write!(quoted, "\\x{byte:02x}");
        }
        quoted.push('"');
        return quoted;
    }
    let plain = !argument.is_empty()
        && !argument
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '\\'));
    if plain {
        return argument.to_owned();
    }
    let mut quoted = String::from("\"");
    for c in argument.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            c if c.is_control() => {
                for byte in c.encode_utf8(&mut [0; 4]).bytes() {
                    let _ = write!(quoted, "\\x{byte:02x}");
                }
            }
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

/// The bytes a key name stands for when it's `\x` and hex for bytes that
/// aren't UTF-8, as replies show them. A name that reads as UTF-8
/// after decoding was typed that way, so it stays as it is.
fn binary_name(name: &str) -> Option<Vec<u8>> {
    let hex = name.strip_prefix("\\x")?;
    if hex.is_empty() || !hex.len().is_multiple_of(2) || !hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("checked hex digits"))
        .collect();
    std::str::from_utf8(&bytes).is_err().then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redis::reply::bytes_text;

    fn args(line: &str) -> Vec<String> {
        split_arguments(line)
            .unwrap()
            .into_iter()
            .map(|arg| String::from_utf8(arg).unwrap())
            .collect()
    }

    #[test]
    fn splits_arguments_like_redis_cli() {
        assert_eq!(args("SET  key   value "), ["SET", "key", "value"]);
        assert_eq!(
            args(r#"SET "a key" "line\nbreak\t\"q\" \\ \x41\x42""#),
            ["SET", "a key", "line\nbreak\t\"q\" \\ AB"]
        );
        assert_eq!(
            args(r"SET 'it\'s \n literal' x"),
            ["SET", r"it's \n literal", "x"]
        );
        assert_eq!(args(r#"SET "" ''"#), ["SET", "", ""]);
        // Quotes may start inside an argument, as in redis-cli.
        assert_eq!(
            args(r#"SET key"with space" v"#),
            ["SET", "keywith space", "v"]
        );
        assert_eq!(args("GET héllo"), ["GET", "héllo"]);
        assert_eq!(
            split_arguments(r#"SET k "\xff\x00""#).unwrap()[2],
            vec![0xff, 0x00]
        );
    }

    #[test]
    fn rejects_bad_quoting() {
        assert_eq!(
            split_arguments(r#"SET k "open"#),
            Err((6, "unbalanced quotes"))
        );
        assert_eq!(
            split_arguments("SET k 'open"),
            Err((6, "unbalanced quotes"))
        );
        assert_eq!(
            split_arguments(r#"SET "a"b"#),
            Err((7, "a closing quote must be followed by a space"))
        );
    }

    #[test]
    fn parses_lines_with_positions() {
        let commands = parse("# comment\n\n  SET a 1\r\nGET a\n   \n").unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(
            commands[0].args,
            [b"SET".to_vec(), b"a".to_vec(), b"1".to_vec()]
        );
        assert_eq!(commands[0].position, 14);
        assert_eq!(commands[1].position, 23);

        match parse("GET a\nSET b \"x") {
            Err(Error::Database { position, .. }) => assert_eq!(position, Some(13)),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn quotes_arguments_that_need_it() {
        assert_eq!(quote("user:1"), "user:1");
        assert_eq!(quote("a key"), r#""a key""#);
        assert_eq!(quote(r#"say "hi"\"#), r#""say \"hi\"\\""#);
        assert_eq!(quote("tab\there\n"), r#""tab\there\n""#);
        assert_eq!(quote(""), r#""""#);
        assert_eq!(quote("\u{1}"), r#""\x01""#);
        // A name standing for bytes that aren't UTF-8 becomes those bytes.
        assert_eq!(quote(&bytes_text(&[0xff, 0x00])), r#""\xff\x00""#);
        // One that decodes to UTF-8 was typed like that.
        assert_eq!(quote(r"\x41"), r#""\\x41""#);
        for name in ["a key", "say \"hi\"", "tab\t", "\u{1}x", "\\x41", "héllo"] {
            assert_eq!(args(&format!("GET {}", quote(name)))[1], name);
        }
        let binary =
            split_arguments(&format!("GET {}", quote(&bytes_text(&[0xff, 0x00])))).unwrap();
        assert_eq!(binary[1], vec![0xff, 0x00]);
    }
}
