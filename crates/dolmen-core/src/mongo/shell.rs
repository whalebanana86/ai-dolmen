//! The part of the mongosh language Dolmen runs: statements such as
//! `db.users.find({ age: { $gt: 30 } }).sort({ name: 1 })`, `show dbs` and
//! `use shop`, read into operations a session can run.
//!
//! Values are JavaScript literals as mongosh takes them (unquoted keys,
//! 'single' quotes, trailing commas, regex literals, `ObjectId("...")` and
//! the other BSON constructors), read straight into BSON. Nothing is
//! evaluated: there are no variables, arithmetic or functions.
//!
//! Positions in errors are 1-based character positions in the text, as for
//! PostgreSQL's syntax errors, so the editor can point at them.

use std::ops::Range;

use mongodb::bson::spec::BinarySubtype;
use mongodb::bson::{self, Binary, Bson, DateTime, Decimal128, Document, Regex, Timestamp};
use mongodb::options::{
    AggregateOptions, CountOptions, DeleteOptions, DistinctOptions, EstimatedDocumentCountOptions,
    InsertManyOptions, InsertOneOptions, ReplaceOptions, UpdateOptions,
};
use serde::de::DeserializeOwned;

use crate::Error;

/// One statement of a script.
#[derive(Debug, Clone)]
pub(crate) struct Statement {
    /// Where it is in the text, in bytes, without the whitespace, comments
    /// and `;` around it.
    pub range: Range<usize>,
    /// The database `db.getSiblingDB("...")` named, instead of the current one.
    pub database: Option<String>,
    pub operation: Operation,
}

// A collection method holds the driver's option structs, so it is large;
// boxing it would gain nothing for the few statements of a script.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub(crate) enum Operation {
    /// `show dbs` or `show databases`.
    ShowDatabases,
    /// `use <database>`.
    Use(String),
    /// A method of `db`, such as `db.runCommand(...)`.
    Database(DatabaseMethod),
    /// A method of a collection, such as `db.users.find(...)`.
    Collection {
        name: String,
        method: CollectionMethod,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum DatabaseMethod {
    /// `db.runCommand(...)`, or `db.adminCommand(...)` with `admin`.
    RunCommand {
        command: Document,
        admin: bool,
    },
    /// `db.getCollectionNames()` and `show collections`.
    GetCollectionNames,
    CreateCollection {
        name: String,
        options: Document,
    },
    CreateView {
        name: String,
        source: String,
        pipeline: Vec<Document>,
        options: Document,
    },
    DropDatabase,
    Stats,
    GetName,
}

#[derive(Debug, Clone)]
pub(crate) enum CollectionMethod {
    Find(Find),
    FindOne {
        filter: Document,
        projection: Option<Document>,
    },
    CountDocuments {
        filter: Document,
        options: Option<CountOptions>,
    },
    EstimatedDocumentCount(Option<EstimatedDocumentCountOptions>),
    Distinct {
        field: String,
        filter: Document,
        options: Option<DistinctOptions>,
    },
    Aggregate {
        pipeline: Vec<Document>,
        options: Option<AggregateOptions>,
    },
    InsertOne {
        document: Document,
        options: Option<InsertOneOptions>,
    },
    InsertMany {
        documents: Vec<Document>,
        options: Option<InsertManyOptions>,
    },
    /// `updateOne`, or `updateMany` with `many`.
    Update {
        filter: Document,
        update: Update,
        options: Option<UpdateOptions>,
        many: bool,
    },
    ReplaceOne {
        filter: Document,
        replacement: Document,
        options: Option<ReplaceOptions>,
    },
    /// `deleteOne`, or `deleteMany` with `many`.
    Delete {
        filter: Document,
        options: Option<DeleteOptions>,
        many: bool,
    },
    /// Options go to the server as written, so it checks them.
    CreateIndex {
        keys: Document,
        options: Document,
    },
    /// An index name, or its keys.
    DropIndex(Bson),
    GetIndexes,
    Drop,
    RenameCollection {
        to: String,
        drop_target: bool,
    },
}

/// `find(filter, projection)` and the cursor methods chained to it.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Find {
    pub filter: Document,
    pub projection: Option<Document>,
    pub sort: Option<Document>,
    pub limit: Option<i64>,
    pub skip: Option<u64>,
    /// Ends in `.count()`: count the matches, ignoring skip and limit as mongosh does.
    pub count: bool,
}

/// An update document such as `{ $set: { a: 1 } }`, or an aggregation pipeline.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Update {
    Document(Document),
    Pipeline(Vec<Document>),
}

impl Operation {
    /// The method that writes, for refusing it on a read-only connection.
    /// Commands and pipelines are checked apart, by what they contain.
    pub fn write_method(&self) -> Option<&'static str> {
        match self {
            Operation::Database(method) => match method {
                DatabaseMethod::CreateCollection { .. } => Some("createCollection"),
                DatabaseMethod::CreateView { .. } => Some("createView"),
                DatabaseMethod::DropDatabase => Some("dropDatabase"),
                _ => None,
            },
            Operation::Collection { method, .. } => match method {
                CollectionMethod::InsertOne { .. } => Some("insertOne"),
                CollectionMethod::InsertMany { .. } => Some("insertMany"),
                CollectionMethod::Update { many: false, .. } => Some("updateOne"),
                CollectionMethod::Update { many: true, .. } => Some("updateMany"),
                CollectionMethod::ReplaceOne { .. } => Some("replaceOne"),
                CollectionMethod::Delete { many: false, .. } => Some("deleteOne"),
                CollectionMethod::Delete { many: true, .. } => Some("deleteMany"),
                CollectionMethod::CreateIndex { .. } => Some("createIndex"),
                CollectionMethod::DropIndex(_) => Some("dropIndex"),
                CollectionMethod::Drop => Some("drop"),
                CollectionMethod::RenameCollection { .. } => Some("renameCollection"),
                _ => None,
            },
            _ => None,
        }
    }
}

/// Reads a script into its statements, or fails at the first mistake
/// without running anything.
pub(crate) fn parse(text: &str) -> Result<Vec<Statement>, Error> {
    match script(text) {
        (statements, None) => Ok(statements),
        (_, Some((_, err))) => Err(err.into_error(text)),
    }
}

/// The byte range of each statement in `text`, in order, as
/// [`Session::execute`](super::Session::execute) splits them: at a `;`, or at
/// a line break once a statement is complete. Ranges leave out whitespace,
/// comments and the `;`. Past a mistake the rest of the text is one last
/// range, so running it shows the error.
pub fn statement_ranges(text: &str) -> Vec<Range<usize>> {
    let (statements, failure) = script(text);
    let mut ranges: Vec<_> = statements.into_iter().map(|s| s.range).collect();
    if let Some((start, _)) = failure {
        ranges.push(start..text.trim_end().len().max(start));
    }
    ranges
}

/// The statements read, and where the statement that failed starts with its error.
fn script(text: &str) -> (Vec<Statement>, Option<(usize, ParseError)>) {
    let mut parser = Parser { text, pos: 0 };
    let mut statements = Vec::new();
    loop {
        // Empty statements, such as `;;`, are skipped.
        loop {
            // An unterminated comment fails from where it starts.
            if let Err(err) = parser.skip_trivia() {
                return (statements, Some((err.pos, err)));
            }
            if !parser.eat(';') {
                break;
            }
        }
        if parser.at_end() {
            return (statements, None);
        }
        let start = parser.pos;
        let (database, operation) = match parser.statement() {
            Ok(statement) => statement,
            Err(err) => return (statements, Some((start, err))),
        };
        let end = parser.pos;
        match parser.skip_trivia() {
            Ok(line_break) if line_break || parser.at_end() || parser.peek() == Some(';') => {}
            Ok(_) => {
                let err = parser.error(parser.pos, "expected ';' or a new line");
                return (statements, Some((start, err)));
            }
            // An unterminated comment on a later line leaves the statement whole.
            Err(err) => {
                if text[end..err.pos].contains(['\n', '\r']) {
                    statements.push(Statement {
                        range: start..end,
                        database,
                        operation,
                    });
                    return (statements, Some((err.pos, err)));
                }
                return (statements, Some((start, err)));
            }
        }
        parser.eat(';');
        statements.push(Statement {
            range: start..end,
            database,
            operation,
        });
    }
}

/// A mistake in the text, at a byte offset.
#[derive(Debug)]
struct ParseError {
    pos: usize,
    message: String,
}

impl ParseError {
    fn into_error(self, text: &str) -> Error {
        Error::Database {
            message: self.message,
            code: "SyntaxError".into(),
            detail: None,
            hint: None,
            position: Some(text[..self.pos].chars().count() as u32 + 1),
        }
    }
}

type Parsed<T> = Result<T, ParseError>;

struct Parser<'a> {
    text: &'a str,
    /// The byte offset read up to.
    pos: usize,
}

/// `db.name`, `db["name"]` or `name(arguments)` in a chain such as
/// `db.users.find({}).limit(5)`.
struct Link {
    name: String,
    pos: usize,
    call: Option<Call>,
}

struct Call {
    arguments: Vec<Argument>,
    /// Where the closing `)` is, to point at when an argument is missing.
    close: usize,
}

struct Argument {
    value: Bson,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn error(&self, pos: usize, message: impl Into<String>) -> ParseError {
        ParseError {
            pos,
            message: message.into(),
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.text.len()
    }

    fn rest(&self) -> &'a str {
        &self.text[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += c.len_utf8();
            true
        } else {
            false
        }
    }

    /// Skips whitespace and comments, and expects `c`.
    fn expect(&mut self, c: char) -> Parsed<()> {
        self.skip_trivia()?;
        if self.eat(c) {
            Ok(())
        } else {
            Err(self.unexpected(&format!("'{c}'")))
        }
    }

    /// "expected <what>", naming what was found instead.
    fn unexpected(&self, what: &str) -> ParseError {
        match self.peek() {
            Some(c) => self.error(self.pos, format!("expected {what}, found '{c}'")),
            None => self.error(self.pos, format!("expected {what}, found the end")),
        }
    }

    /// Skips whitespace, `//` and `/* */` comments, and says whether a line
    /// break was among them; a block comment spanning lines counts, as in JavaScript.
    fn skip_trivia(&mut self) -> Parsed<bool> {
        let mut line_break = false;
        loop {
            let rest = self.rest();
            if rest.starts_with("//") {
                self.pos += rest.find('\n').unwrap_or(rest.len());
            } else if let Some(comment) = rest.strip_prefix("/*") {
                let Some(end) = comment.find("*/") else {
                    return Err(self.error(self.pos, "unterminated comment"));
                };
                line_break |= comment[..end].contains(['\n', '\r']);
                self.pos += end + 4;
            } else {
                match self.peek() {
                    Some('\n' | '\r' | '\u{2028}' | '\u{2029}') => line_break = true,
                    Some(c) if c.is_whitespace() => {}
                    _ => return Ok(line_break),
                }
                self.bump();
            }
        }
    }

    fn identifier(&mut self) -> Option<&'a str> {
        let rest = self.rest();
        if !rest.starts_with(is_identifier_start) {
            return None;
        }
        let end = rest
            .char_indices()
            .find(|&(_, c)| !is_identifier_part(c))
            .map_or(rest.len(), |(i, _)| i);
        self.pos += end;
        Some(&rest[..end])
    }

    /// The text after spaces on the same line, up to whitespace or `;`, for
    /// `use` and `show`, and where it starts.
    fn word(&mut self) -> (usize, &'a str) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.bump();
        }
        let start = self.pos;
        let rest = self.rest();
        let end = rest
            .find(|c: char| c.is_whitespace() || c == ';')
            .unwrap_or(rest.len());
        self.pos += end;
        (start, &rest[..end])
    }

    fn statement(&mut self) -> Parsed<(Option<String>, Operation)> {
        let start = self.pos;
        let keyword = self.identifier();
        let spaced = matches!(self.peek(), Some(' ' | '\t'));
        match keyword {
            Some("db") => self.chain(),
            Some("use") if spaced => {
                let (pos, name) = self.word();
                if name.is_empty() {
                    return Err(self.error(pos, "expected a database name after use"));
                }
                Ok((None, Operation::Use(name.to_owned())))
            }
            Some("show") if spaced => {
                let (pos, word) = self.word();
                let operation = match word {
                    "dbs" | "databases" => Operation::ShowDatabases,
                    "collections" | "tables" => {
                        Operation::Database(DatabaseMethod::GetCollectionNames)
                    }
                    _ => {
                        return Err(
                            self.error(pos, "expected dbs, databases or collections after show")
                        );
                    }
                };
                Ok((None, operation))
            }
            _ => Err(self.error(start, "expected a statement starting with db, show or use")),
        }
    }

    /// Reads `db` followed by names and calls, such as `.users.find({})`. The
    /// statement is complete after a call; a line starting with `.` still
    /// continues it, as in JavaScript.
    fn chain(&mut self) -> Parsed<(Option<String>, Operation)> {
        let mut links: Vec<Link> = Vec::new();
        loop {
            let complete = links.last().is_some_and(|link| link.call.is_some());
            let before = self.pos;
            self.skip_trivia()?;
            if self.eat('.') {
                self.skip_trivia()?;
                let pos = self.pos;
                let Some(name) = self.identifier() else {
                    return Err(self.unexpected("a name after '.'"));
                };
                links.push(Link {
                    name: name.to_owned(),
                    pos,
                    call: None,
                });
            } else if !complete && self.peek() == Some('[') {
                self.bump();
                self.skip_trivia()?;
                let pos = self.pos;
                let name = match self.peek() {
                    Some('"' | '\'') => self.string()?,
                    _ => return Err(self.unexpected("a quoted collection name")),
                };
                self.expect(']')?;
                links.push(Link {
                    name,
                    pos,
                    call: None,
                });
            } else if !complete && self.peek() == Some('(') && !links.is_empty() {
                let call = self.call()?;
                links.last_mut().expect("checked above").call = Some(call);
            } else if complete {
                self.pos = before;
                break;
            } else if links.is_empty() {
                return Err(self.unexpected("'.' after db"));
            } else {
                return Err(self.unexpected("'(' to call a method"));
            }
        }
        interpret(self, links)
    }

    /// Reads `(value, value, ...)`.
    fn call(&mut self) -> Parsed<Call> {
        self.expect('(')?;
        let mut arguments = Vec::new();
        loop {
            self.skip_trivia()?;
            if self.peek() == Some(')') {
                break;
            }
            let pos = self.pos;
            let value = self.value()?;
            arguments.push(Argument { value, pos });
            self.skip_trivia()?;
            if !self.eat(',') {
                break;
            }
        }
        self.skip_trivia()?;
        let close = self.pos;
        if !self.eat(')') {
            return Err(self.unexpected("',' or ')'"));
        }
        Ok(Call { arguments, close })
    }

    fn value(&mut self) -> Parsed<Bson> {
        self.skip_trivia()?;
        match self.peek() {
            Some('{') => self.object().map(Bson::Document),
            Some('[') => self.array(),
            Some('"' | '\'') => self.string().map(Bson::String),
            Some('/') => self.regex(),
            Some(c) if c.is_ascii_digit() || matches!(c, '-' | '+' | '.') => self.number(),
            Some(c) if is_identifier_start(c) => self.word_value(),
            _ => Err(self.unexpected("a value")),
        }
    }

    fn object(&mut self) -> Parsed<Document> {
        self.expect('{')?;
        let mut document = Document::new();
        loop {
            self.skip_trivia()?;
            if self.eat('}') {
                return Ok(document);
            }
            let key = match self.peek() {
                Some('"' | '\'') => self.string()?,
                Some(c) if c.is_ascii_digit() => {
                    let start = self.pos;
                    while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                        self.bump();
                    }
                    self.text[start..self.pos].to_owned()
                }
                _ => match self.identifier() {
                    Some(name) => name.to_owned(),
                    None => return Err(self.unexpected("a field name or '}'")),
                },
            };
            self.expect(':')?;
            let value = self.value()?;
            document.insert(key, value);
            self.skip_trivia()?;
            if !self.eat(',') {
                self.skip_trivia()?;
                if self.eat('}') {
                    return Ok(document);
                }
                return Err(self.unexpected("',' or '}'"));
            }
        }
    }

    fn array(&mut self) -> Parsed<Bson> {
        self.expect('[')?;
        let mut values = Vec::new();
        loop {
            self.skip_trivia()?;
            if self.eat(']') {
                return Ok(Bson::Array(values));
            }
            values.push(self.value()?);
            self.skip_trivia()?;
            if !self.eat(',') {
                self.skip_trivia()?;
                if self.eat(']') {
                    return Ok(Bson::Array(values));
                }
                return Err(self.unexpected("',' or ']'"));
            }
        }
    }

    /// A 'single' or "double" quoted string, with JavaScript's escapes.
    fn string(&mut self) -> Parsed<String> {
        let start = self.pos;
        let quote = self.bump().expect("called at a quote");
        let mut out = String::new();
        loop {
            let pos = self.pos;
            match self.bump() {
                None | Some('\n' | '\r') => {
                    return Err(self.error(start, "unterminated string"));
                }
                Some(c) if c == quote => return Ok(out),
                Some('\\') => match self.bump() {
                    None => return Err(self.error(start, "unterminated string")),
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('b') => out.push('\u{8}'),
                    Some('f') => out.push('\u{c}'),
                    Some('v') => out.push('\u{b}'),
                    Some('0') => out.push('\0'),
                    Some('x') => {
                        let code = self.hex_digits(2, pos)?;
                        out.push(char::from_u32(code).expect("two hex digits"));
                    }
                    Some('u') => out.push(self.unicode_escape(pos)?),
                    // A backslash at the end of a line continues the string.
                    Some('\r') => {
                        self.eat('\n');
                    }
                    Some('\n') => {}
                    Some(c) => out.push(c),
                },
                Some(c) => out.push(c),
            }
        }
    }

    fn hex_digits(&mut self, count: usize, escape: usize) -> Parsed<u32> {
        let digits = self.rest().get(..count).unwrap_or("");
        match u32::from_str_radix(digits, 16) {
            Ok(code) if digits.chars().all(|c| c.is_ascii_hexdigit()) => {
                self.pos += count;
                Ok(code)
            }
            _ => Err(self.error(escape, "invalid escape sequence")),
        }
    }

    /// `\uXXXX`, `\u{X...}`, or a surrogate pair of `\uXXXX`, after the `\u`.
    fn unicode_escape(&mut self, escape: usize) -> Parsed<char> {
        let invalid = |parser: &Self| parser.error(escape, "invalid unicode escape");
        if self.eat('{') {
            let end = self.rest().find('}').ok_or_else(|| invalid(self))?;
            let code = u32::from_str_radix(&self.rest()[..end], 16).map_err(|_| invalid(self))?;
            self.pos += end + 1;
            return char::from_u32(code).ok_or_else(|| invalid(self));
        }
        let high = self.hex_digits(4, escape)?;
        if (0xD800..0xDC00).contains(&high) && self.rest().starts_with("\\u") {
            self.pos += 2;
            let low = self.hex_digits(4, escape)?;
            if (0xDC00..0xE000).contains(&low) {
                let code = 0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00);
                return char::from_u32(code).ok_or_else(|| invalid(self));
            }
        }
        char::from_u32(high).ok_or_else(|| invalid(self))
    }

    /// A regex literal such as `/^ab+/i`.
    fn regex(&mut self) -> Parsed<Bson> {
        let start = self.pos;
        self.bump();
        let mut pattern = String::new();
        let mut in_class = false;
        loop {
            match self.bump() {
                None | Some('\n' | '\r') => {
                    return Err(self.error(start, "unterminated regular expression"));
                }
                Some('\\') => {
                    pattern.push('\\');
                    match self.bump() {
                        Some(c) if c != '\n' && c != '\r' => pattern.push(c),
                        _ => return Err(self.error(start, "unterminated regular expression")),
                    }
                }
                Some('/') if !in_class => break,
                Some(c) => {
                    in_class = match c {
                        '[' => true,
                        ']' => false,
                        _ => in_class,
                    };
                    pattern.push(c);
                }
            }
        }
        let flags_start = self.pos;
        let flags = self.identifier().unwrap_or("");
        regex(pattern, flags).map_err(|message| self.error(flags_start, message))
    }

    /// An integer becomes an Int32 when it fits, else an Int64; a fraction
    /// or an exponent makes a Double, as do integers past 64 bits.
    fn number(&mut self) -> Parsed<Bson> {
        let start = self.pos;
        let negative = self.eat('-');
        if !negative {
            self.eat('+');
        }
        let sign = if negative { -1.0 } else { 1.0 };
        if self.rest().starts_with("Infinity") {
            self.pos += "Infinity".len();
            return Ok(Bson::Double(sign * f64::INFINITY));
        }
        if self.rest().starts_with("NaN") {
            self.pos += "NaN".len();
            return Ok(Bson::Double(f64::NAN));
        }
        let digits_start = self.pos;
        let value = if self.rest().starts_with("0x") || self.rest().starts_with("0X") {
            self.pos += 2;
            let hex_start = self.pos;
            while self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                self.bump();
            }
            let hex = &self.text[hex_start..self.pos];
            match i128::from_str_radix(hex, 16) {
                Ok(value) => integer(if negative { -value } else { value }),
                Err(_) => return Err(self.error(start, "invalid number")),
            }
        } else {
            let mut fraction = false;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.bump();
            }
            if self.eat('.') {
                fraction = true;
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.bump();
                }
            }
            if matches!(self.peek(), Some('e' | 'E')) {
                fraction = true;
                self.bump();
                if !self.eat('-') {
                    self.eat('+');
                }
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.bump();
                }
            }
            let literal = &self.text[digits_start..self.pos];
            let whole = literal.parse::<i128>();
            match (fraction, whole) {
                (false, Ok(value)) => integer(if negative { -value } else { value }),
                _ => match literal.parse::<f64>() {
                    Ok(value) if literal != "." => Bson::Double(sign * value),
                    _ => return Err(self.error(start, "invalid number")),
                },
            }
        };
        if self.peek().is_some_and(is_identifier_part) {
            return Err(self.error(start, "invalid number"));
        }
        Ok(value)
    }

    /// `true`, `null`, `ObjectId("...")` and the other words a value can be.
    fn word_value(&mut self) -> Parsed<Bson> {
        let start = self.pos;
        let mut word = self.identifier().expect("called at an identifier");
        let mut constructed = false;
        if word == "new" {
            self.skip_trivia()?;
            word = match self.identifier() {
                Some(word) => word,
                None => return Err(self.unexpected("a constructor after new")),
            };
            constructed = true;
        }
        let simple = match word {
            "true" => Some(Bson::Boolean(true)),
            "false" => Some(Bson::Boolean(false)),
            "null" | "undefined" => Some(Bson::Null),
            "NaN" => Some(Bson::Double(f64::NAN)),
            "Infinity" => Some(Bson::Double(f64::INFINITY)),
            _ => None,
        };
        if let Some(value) = simple.filter(|_| !constructed) {
            return Ok(value);
        }
        let known = [
            "ObjectId",
            "ISODate",
            "Date",
            "NumberInt",
            "NumberLong",
            "NumberDecimal",
            "Decimal128",
            "UUID",
            "Timestamp",
            "BinData",
            "MinKey",
            "MaxKey",
            "RegExp",
        ];
        if !known.contains(&word) {
            return Err(self.error(start, format!("unknown value {word}")));
        }
        // MinKey and MaxKey also stand alone, as in mongosh.
        let before = self.pos;
        self.skip_trivia()?;
        if self.peek() != Some('(') && matches!(word, "MinKey" | "MaxKey") {
            self.pos = before;
            return Ok(if word == "MinKey" {
                Bson::MinKey
            } else {
                Bson::MaxKey
            });
        }
        let call = self.call()?;
        construct(word, call).map_err(|(pos, message)| self.error(pos.unwrap_or(start), message))
    }
}

fn is_identifier_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$'
}

fn is_identifier_part(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

fn integer(value: i128) -> Bson {
    if let Ok(value) = i32::try_from(value) {
        Bson::Int32(value)
    } else if let Ok(value) = i64::try_from(value) {
        Bson::Int64(value)
    } else {
        Bson::Double(value as f64)
    }
}

/// A BSON regex from a JavaScript one. BSON keeps the flags sorted, and
/// only knows those MongoDB's regex engine takes.
fn regex(pattern: String, flags: &str) -> Result<Bson, String> {
    let mut options: Vec<char> = flags.chars().collect();
    if let Some(flag) = options.iter().find(|c| !"imsux".contains(**c)) {
        return Err(format!("regular expression flag {flag} isn't supported"));
    }
    options.sort_unstable();
    options.dedup();
    Ok(Bson::RegularExpression(Regex {
        pattern,
        options: options.into_iter().collect(),
    }))
}

/// Builds the value of a constructor such as `ObjectId("...")`. Errors
/// carry the position of the argument at fault, or none for the constructor.
fn construct(name: &str, call: Call) -> Result<Bson, (Option<usize>, String)> {
    let count = call.arguments.len();
    let mut arguments = call.arguments.into_iter();
    let first = arguments.next();
    let second = arguments.next();
    if arguments.next().is_some()
        || (second.is_some() && !matches!(name, "Timestamp" | "BinData" | "RegExp"))
    {
        return Err((None, format!("too many arguments to {name}()")));
    }
    let at = |argument: &Option<Argument>| argument.as_ref().map(|a| a.pos);
    let bad = |argument: &Option<Argument>, message: &str| Err((at(argument), message.to_owned()));

    let text = |argument: &Option<Argument>| match argument.as_ref().map(|a| &a.value) {
        Some(Bson::String(text)) => Some(text.clone()),
        _ => None,
    };
    let whole = |argument: &Option<Argument>| -> Option<i64> {
        match argument.as_ref().map(|a| &a.value)? {
            Bson::Int32(n) => Some(i64::from(*n)),
            Bson::Int64(n) => Some(*n),
            Bson::Double(n) if n.is_finite() => Some(n.trunc() as i64),
            Bson::String(text) => text.trim().parse().ok(),
            _ => None,
        }
    };

    match name {
        "ObjectId" => match (&first, text(&first)) {
            (None, _) => Ok(Bson::ObjectId(bson::oid::ObjectId::new())),
            (_, Some(hex)) => match bson::oid::ObjectId::parse_str(&hex) {
                Ok(id) => Ok(Bson::ObjectId(id)),
                Err(_) => bad(&first, "an ObjectId is 24 hexadecimal digits"),
            },
            _ => bad(&first, "an ObjectId is 24 hexadecimal digits"),
        },
        "ISODate" | "Date" => match first.as_ref().map(|a| &a.value) {
            None => Ok(Bson::DateTime(DateTime::now())),
            Some(Bson::String(text)) => match parse_date(text) {
                Some(date) => Ok(Bson::DateTime(date)),
                None => bad(
                    &first,
                    "expected an ISO-8601 date such as 2024-05-01T12:00:00Z",
                ),
            },
            Some(Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_)) if name == "Date" => Ok(
                Bson::DateTime(DateTime::from_millis(whole(&first).unwrap_or(0))),
            ),
            _ => bad(&first, "expected a date string"),
        },
        "NumberInt" => match whole(&first).map(i32::try_from) {
            Some(Ok(value)) => Ok(Bson::Int32(value)),
            _ if first.is_none() => Ok(Bson::Int32(0)),
            _ => bad(&first, "expected a 32-bit integer"),
        },
        "NumberLong" => match whole(&first) {
            Some(value) => Ok(Bson::Int64(value)),
            None if first.is_none() => Ok(Bson::Int64(0)),
            None => bad(&first, "expected a 64-bit integer"),
        },
        "NumberDecimal" | "Decimal128" => {
            let literal = match first.as_ref().map(|a| &a.value) {
                Some(Bson::String(text)) => text.clone(),
                Some(Bson::Int32(n)) => n.to_string(),
                Some(Bson::Int64(n)) => n.to_string(),
                Some(Bson::Double(n)) => n.to_string(),
                None => "0".to_owned(),
                _ => return bad(&first, "expected a decimal number"),
            };
            match literal.trim().parse::<Decimal128>() {
                Ok(value) => Ok(Bson::Decimal128(value)),
                Err(_) => bad(&first, "expected a decimal number"),
            }
        }
        "UUID" => match (&first, text(&first)) {
            (None, _) => Ok(Bson::Binary(Binary::from_uuid(bson::Uuid::new()))),
            (_, Some(text)) => match bson::Uuid::parse_str(&text) {
                Ok(uuid) => Ok(Bson::Binary(Binary::from_uuid(uuid))),
                Err(_) => bad(
                    &first,
                    "expected a UUID such as 01234567-89ab-cdef-0123-456789abcdef",
                ),
            },
            _ => bad(&first, "expected a UUID string"),
        },
        "Timestamp" => {
            let (time, increment) = match first.as_ref().map(|a| &a.value) {
                None => (Some(0), Some(0)),
                // Timestamp({ t: 1, i: 2 }), as newer mongosh writes it.
                Some(Bson::Document(parts)) if count == 1 => {
                    let part = |key| {
                        parts
                            .get(key)
                            .cloned()
                            .map(|value| Argument { value, pos: 0 })
                    };
                    (whole(&part("t")), whole(&part("i")))
                }
                _ => (
                    whole(&first),
                    whole(&second).or(second.is_none().then_some(0)),
                ),
            };
            match (time.map(u32::try_from), increment.map(u32::try_from)) {
                (Some(Ok(time)), Some(Ok(increment))) => {
                    Ok(Bson::Timestamp(Timestamp { time, increment }))
                }
                _ => bad(&first, "expected Timestamp(seconds, increment)"),
            }
        }
        "BinData" => {
            let subtype = whole(&first).and_then(|n| u8::try_from(n).ok());
            match (subtype, text(&second)) {
                (Some(subtype), Some(base64)) => {
                    match Binary::from_base64(base64, BinarySubtype::from(subtype)) {
                        Ok(binary) => Ok(Bson::Binary(binary)),
                        Err(_) => bad(&second, "expected base64 data"),
                    }
                }
                (None, _) => bad(&first, "expected a binary subtype from 0 to 255"),
                (_, None) => bad(&second, "expected base64 data"),
            }
        }
        "MinKey" if first.is_none() => Ok(Bson::MinKey),
        "MaxKey" if first.is_none() => Ok(Bson::MaxKey),
        "RegExp" => match (text(&first), &second) {
            (Some(pattern), None) => regex(pattern, "").map_err(|m| (at(&first), m)),
            (Some(pattern), Some(_)) => match text(&second) {
                Some(flags) => regex(pattern, &flags).map_err(|m| (at(&second), m)),
                None => bad(&second, "expected flags such as \"i\""),
            },
            _ => bad(&first, "expected a pattern string"),
        },
        _ => Err((at(&first), format!("too many arguments to {name}()"))),
    }
}

/// Reads an ISO-8601 date as mongosh's `ISODate` takes it: `2024-05-01`,
/// `2024-05-01T10:30`, `2024-05-01 10:30:00.123+09:00` and the like. A time
/// without a zone is UTC.
pub(crate) fn parse_date(text: &str) -> Option<DateTime> {
    let bytes = text.trim().as_bytes();
    let mut i = 0;
    let digits = |i: &mut usize, count: usize| -> Option<i64> {
        let part = bytes.get(*i..*i + count)?;
        if !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        *i += count;
        std::str::from_utf8(part).ok()?.parse().ok()
    };
    let year = digits(&mut i, 4)?;
    let dashes = bytes.get(i) == Some(&b'-');
    i += usize::from(dashes);
    let month = digits(&mut i, 2)?;
    if dashes {
        (bytes.get(i) == Some(&b'-')).then_some(())?;
        i += 1;
    }
    let day = digits(&mut i, 2)?;
    let (mut hour, mut minute, mut second, mut millis, mut offset) = (0, 0, 0, 0, 0);
    if matches!(bytes.get(i), Some(b'T' | b't' | b' ')) {
        i += 1;
        hour = digits(&mut i, 2)?;
        if bytes.get(i) == Some(&b':') {
            i += 1;
            minute = digits(&mut i, 2)?;
            if bytes.get(i) == Some(&b':') {
                i += 1;
                second = digits(&mut i, 2)?;
                if bytes.get(i) == Some(&b'.') {
                    i += 1;
                    let start = i;
                    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                        i += 1;
                    }
                    let fraction = std::str::from_utf8(&bytes[start..i]).ok()?;
                    if fraction.is_empty() {
                        return None;
                    }
                    millis = format!("{:0<3}", &fraction[..fraction.len().min(3)])
                        .parse()
                        .ok()?;
                }
            }
        }
        match bytes.get(i) {
            Some(b'Z' | b'z') => i += 1,
            Some(&sign @ (b'+' | b'-')) => {
                i += 1;
                let hours = digits(&mut i, 2)?;
                i += usize::from(bytes.get(i) == Some(&b':'));
                let minutes = if i < bytes.len() {
                    digits(&mut i, 2)?
                } else {
                    0
                };
                offset = (hours * 60 + minutes) * if sign == b'-' { -1 } else { 1 };
            }
            _ => {}
        }
    }
    if i != bytes.len() || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    // A day past the end of its month comes back as another date.
    if civil_from_days(days) != (year, month, day) || !(1..=12).contains(&month) {
        return None;
    }
    let seconds = ((days * 24 + hour) * 60 + minute - offset) * 60 + second;
    Some(DateTime::from_millis(seconds * 1000 + millis))
}

/// A date as ISO-8601 in UTC with milliseconds, e.g. `2024-05-01T10:30:00.000Z`,
/// or `None` for years outside 0 to 9999, which ISO-8601 writes differently.
pub(crate) fn format_date(date: DateTime) -> Option<String> {
    let millis = date.timestamp_millis();
    let (year, month, day) = civil_from_days(millis.div_euclid(86_400_000));
    if !(0..=9999).contains(&year) {
        return None;
    }
    let time = millis.rem_euclid(86_400_000);
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        time / 3_600_000,
        time / 60_000 % 60,
        time / 1000 % 60,
        time % 1000
    ))
}

/// Days since 1970-01-01 of a date in the proleptic Gregorian calendar
/// (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The date of a day since 1970-01-01, as (year, month, day).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Turns the names and calls after `db` into an operation, checking the
/// methods and their arguments.
fn interpret(parser: &Parser, links: Vec<Link>) -> Parsed<(Option<String>, Operation)> {
    let mut links = links.into_iter().peekable();
    let mut database = None;
    if let Some(link) = links.next_if(|l| l.name == "getSiblingDB" && l.call.is_some()) {
        let mut arguments = Arguments::new(parser, &link);
        database = Some(arguments.string("a database name")?);
        arguments.end()?;
    }

    let collection = match links.next_if(|l| l.name == "getCollection" && l.call.is_some()) {
        Some(link) => {
            let mut arguments = Arguments::new(parser, &link);
            let name = arguments.string("a collection name")?;
            arguments.end()?;
            Some(name)
        }
        None => {
            // `db.system.views.find()`: the names before the first call are the collection's.
            let mut names = Vec::new();
            while let Some(link) = links.next_if(|l| l.call.is_none()) {
                names.push(link.name);
            }
            (!names.is_empty()).then(|| names.join("."))
        }
    };
    let Some(method) = links.next() else {
        return Err(parser.error(parser.pos, "expected a method call"));
    };
    let chain: Vec<Link> = links.collect();
    if let Some(link) = chain.iter().find(|l| l.call.is_none()) {
        return Err(parser.error(
            link.pos,
            format!("expected {}() to be a method call", link.name),
        ));
    }

    let operation = match collection {
        None => {
            refuse_chain(parser, &method, &chain, &[])?;
            Operation::Database(database_method(parser, &method)?)
        }
        Some(name) => Operation::Collection {
            method: collection_method(parser, &method, &chain)?,
            name,
        },
    };
    Ok((database, operation))
}

/// Fails at the first chained call that isn't among `allowed`.
fn refuse_chain(parser: &Parser, method: &Link, chain: &[Link], allowed: &[&str]) -> Parsed<()> {
    for link in chain {
        if !allowed.contains(&link.name.as_str()) {
            return Err(parser.error(
                link.pos,
                format!("{}() can't follow {}()", link.name, method.name),
            ));
        }
        Arguments::new(parser, link).end()?;
    }
    Ok(())
}

fn database_method(parser: &Parser, link: &Link) -> Parsed<DatabaseMethod> {
    let mut arguments = Arguments::new(parser, link);
    let method = match link.name.as_str() {
        "runCommand" | "adminCommand" => DatabaseMethod::RunCommand {
            command: arguments.command()?,
            admin: link.name == "adminCommand",
        },
        "getCollectionNames" => DatabaseMethod::GetCollectionNames,
        "createCollection" => DatabaseMethod::CreateCollection {
            name: arguments.string("a collection name")?,
            options: arguments.optional_document("options")?.unwrap_or_default(),
        },
        "createView" => DatabaseMethod::CreateView {
            name: arguments.string("a view name")?,
            source: arguments.string("the collection the view reads")?,
            pipeline: arguments.pipeline()?,
            options: arguments.optional_document("options")?.unwrap_or_default(),
        },
        "dropDatabase" => DatabaseMethod::DropDatabase,
        "stats" => DatabaseMethod::Stats,
        "getName" => DatabaseMethod::GetName,
        other => {
            return Err(parser.error(
                link.pos,
                format!("db.{other}() isn't a database method Dolmen knows"),
            ));
        }
    };
    arguments.end()?;
    Ok(method)
}

fn collection_method(parser: &Parser, link: &Link, chain: &[Link]) -> Parsed<CollectionMethod> {
    let mut arguments = Arguments::new(parser, link);
    let method = match link.name.as_str() {
        "find" => {
            let mut find = Find {
                filter: arguments.optional_document("a filter")?.unwrap_or_default(),
                projection: arguments.optional_document("a projection")?,
                ..Find::default()
            };
            arguments.end()?;
            for cursor in chain {
                if find.count {
                    return Err(parser.error(cursor.pos, "count() must come last"));
                }
                let mut arguments = Arguments::new(parser, cursor);
                match cursor.name.as_str() {
                    "sort" => find.sort = Some(arguments.document("a sort order")?),
                    "projection" => find.projection = Some(arguments.document("a projection")?),
                    "limit" => find.limit = Some(arguments.integer("a number")?),
                    "skip" => {
                        let (skip, pos) = (arguments.integer("a number")?, cursor.pos);
                        find.skip = Some(u64::try_from(skip).map_err(|_| {
                            parser.error(pos, "skip() takes a number that isn't negative")
                        })?);
                    }
                    "count" => {
                        // count(applySkipLimit) is accepted, as mongosh does, and ignored.
                        arguments.next();
                        find.count = true;
                    }
                    "toArray" | "pretty" => {}
                    other => {
                        return Err(parser.error(
                            cursor.pos,
                            format!("{other}() isn't a cursor method Dolmen knows"),
                        ));
                    }
                }
                arguments.end()?;
            }
            return Ok(CollectionMethod::Find(find));
        }
        "findOne" => CollectionMethod::FindOne {
            filter: arguments.optional_document("a filter")?.unwrap_or_default(),
            projection: arguments.optional_document("a projection")?,
        },
        "countDocuments" => CollectionMethod::CountDocuments {
            filter: arguments.optional_document("a filter")?.unwrap_or_default(),
            options: arguments.options()?,
        },
        "estimatedDocumentCount" => CollectionMethod::EstimatedDocumentCount(arguments.options()?),
        "distinct" => CollectionMethod::Distinct {
            field: arguments.string("a field name")?,
            filter: arguments.optional_document("a filter")?.unwrap_or_default(),
            options: arguments.options()?,
        },
        "aggregate" => {
            // aggregate([stages], options), or the stages one by one as mongosh allows.
            let stages_listed = matches!(arguments.peek(), Some(Bson::Document(_)));
            let (pipeline, options) = if stages_listed {
                let mut pipeline = Vec::new();
                while arguments.peek().is_some() {
                    pipeline.push(arguments.document("a pipeline stage")?);
                }
                (pipeline, None)
            } else {
                (arguments.pipeline()?, arguments.options()?)
            };
            refuse_chain(parser, link, chain, &["toArray", "pretty"])?;
            arguments.end()?;
            return Ok(CollectionMethod::Aggregate { pipeline, options });
        }
        "insertOne" => CollectionMethod::InsertOne {
            document: arguments.document("a document")?,
            options: arguments.options()?,
        },
        "insertMany" => CollectionMethod::InsertMany {
            documents: arguments.documents("documents")?,
            options: arguments.options()?,
        },
        "updateOne" | "updateMany" => {
            let filter = arguments.document("a filter")?;
            let update = match arguments.peek() {
                Some(Bson::Array(_)) => Update::Pipeline(arguments.pipeline()?),
                _ => Update::Document(arguments.document("an update")?),
            };
            CollectionMethod::Update {
                filter,
                update,
                options: arguments.options()?,
                many: link.name == "updateMany",
            }
        }
        "replaceOne" => CollectionMethod::ReplaceOne {
            filter: arguments.document("a filter")?,
            replacement: arguments.document("a replacement document")?,
            options: arguments.options()?,
        },
        "deleteOne" | "deleteMany" => CollectionMethod::Delete {
            filter: arguments.document("a filter")?,
            options: arguments.options()?,
            many: link.name == "deleteMany",
        },
        "createIndex" => CollectionMethod::CreateIndex {
            keys: arguments.document("index keys")?,
            options: arguments.optional_document("options")?.unwrap_or_default(),
        },
        "dropIndex" => match arguments.next() {
            Some(Argument {
                value: value @ (Bson::String(_) | Bson::Document(_)),
                ..
            }) => CollectionMethod::DropIndex(value),
            Some(argument) => {
                return Err(parser.error(argument.pos, "expected an index name or its keys"));
            }
            None => return Err(arguments.missing("an index name or its keys")),
        },
        "getIndexes" => CollectionMethod::GetIndexes,
        "drop" => CollectionMethod::Drop,
        "renameCollection" => CollectionMethod::RenameCollection {
            to: arguments.string("the new name")?,
            drop_target: match arguments.next() {
                None => false,
                Some(Argument {
                    value: Bson::Boolean(drop_target),
                    ..
                }) => drop_target,
                Some(argument) => {
                    return Err(parser.error(argument.pos, "expected true or false"));
                }
            },
        },
        other => {
            return Err(parser.error(
                link.pos,
                format!("{other}() isn't a collection method Dolmen knows"),
            ));
        }
    };
    refuse_chain(parser, link, chain, &[])?;
    arguments.end()?;
    Ok(method)
}

/// The arguments of a call, taken in order with their checks.
struct Arguments<'p, 'a> {
    parser: &'p Parser<'a>,
    method: &'p str,
    close: usize,
    values: std::iter::Peekable<std::vec::IntoIter<Argument>>,
}

impl<'p, 'a> Arguments<'p, 'a> {
    fn new(parser: &'p Parser<'a>, link: &'p Link) -> Self {
        let call = link.call.as_ref().expect("a call");
        Self {
            parser,
            method: &link.name,
            close: call.close,
            values: call
                .arguments
                .iter()
                .map(|a| Argument {
                    value: a.value.clone(),
                    pos: a.pos,
                })
                .collect::<Vec<_>>()
                .into_iter()
                .peekable(),
        }
    }

    fn peek(&mut self) -> Option<&Bson> {
        self.values.peek().map(|a| &a.value)
    }

    fn next(&mut self) -> Option<Argument> {
        self.values.next()
    }

    fn missing(&self, what: &str) -> ParseError {
        self.parser
            .error(self.close, format!("{}() needs {what}", self.method))
    }

    fn wrong(&self, argument: &Argument, what: &str) -> ParseError {
        self.parser.error(argument.pos, format!("expected {what}"))
    }

    fn document(&mut self, what: &str) -> Parsed<Document> {
        match self.next() {
            Some(Argument {
                value: Bson::Document(document),
                ..
            }) => Ok(document),
            Some(argument) => Err(self.wrong(&argument, &format!("{what}, as a document"))),
            None => Err(self.missing(what)),
        }
    }

    /// A document, or nothing; `null` and `undefined` count as nothing.
    fn optional_document(&mut self, what: &str) -> Parsed<Option<Document>> {
        match self.peek() {
            None => Ok(None),
            Some(Bson::Null) => {
                self.next();
                Ok(None)
            }
            Some(_) => self.document(what).map(Some),
        }
    }

    fn documents(&mut self, what: &str) -> Parsed<Vec<Document>> {
        match self.next() {
            Some(Argument {
                value: Bson::Array(values),
                pos,
            }) => values
                .into_iter()
                .map(|value| match value {
                    Bson::Document(document) => Ok(document),
                    _ => Err(self
                        .parser
                        .error(pos, format!("{what} must all be documents"))),
                })
                .collect(),
            Some(argument) => Err(self.wrong(&argument, &format!("{what} in an array"))),
            None => Err(self.missing(what)),
        }
    }

    fn pipeline(&mut self) -> Parsed<Vec<Document>> {
        self.documents("pipeline stages")
    }

    fn string(&mut self, what: &str) -> Parsed<String> {
        match self.next() {
            Some(Argument {
                value: Bson::String(text),
                ..
            }) => Ok(text),
            Some(argument) => Err(self.wrong(&argument, &format!("{what}, as a string"))),
            None => Err(self.missing(what)),
        }
    }

    fn integer(&mut self, what: &str) -> Parsed<i64> {
        match self.next() {
            Some(Argument { value, pos }) => match value {
                Bson::Int32(n) => Ok(i64::from(n)),
                Bson::Int64(n) => Ok(n),
                Bson::Double(n) if n.fract() == 0.0 && n.is_finite() => Ok(n as i64),
                _ => Err(self.parser.error(pos, format!("expected {what}"))),
            },
            None => Err(self.missing(what)),
        }
    }

    /// `runCommand`'s command: a document, or a name such as `"ping"`.
    fn command(&mut self) -> Parsed<Document> {
        match self.peek() {
            Some(Bson::String(_)) => {
                let name = self.string("a command")?;
                let mut command = Document::new();
                command.insert(name, 1);
                Ok(command)
            }
            _ => self.document("a command"),
        }
    }

    /// Options the driver takes, such as `{ upsert: true }`.
    fn options<T: DeserializeOwned>(&mut self) -> Parsed<Option<T>> {
        let pos = self.values.peek().map(|a| a.pos);
        let Some(options) = self.optional_document("options")? else {
            return Ok(None);
        };
        bson::from_document(options).map(Some).map_err(|err| {
            self.parser
                .error(pos.unwrap_or(self.close), format!("invalid options: {err}"))
        })
    }

    /// Fails if any argument is left over.
    fn end(mut self) -> Parsed<()> {
        match self.values.next() {
            Some(argument) => Err(self.parser.error(
                argument.pos,
                format!("too many arguments to {}()", self.method),
            )),
            None => Ok(()),
        }
    }
}

/// A value written the way mongosh would take it back, such as
/// `{ email: 1, "a.b": -1 }` or `ISODate("2024-05-01T00:00:00.000Z")`.
pub(crate) fn to_shell(value: &Bson) -> String {
    let mut out = String::new();
    write_shell(&mut out, value);
    out
}

/// A string in double quotes, with JSON's escapes.
pub(crate) fn quote(text: &str) -> String {
    serde_json::to_string(text).expect("strings always serialize")
}

fn write_shell(out: &mut String, value: &Bson) {
    match value {
        Bson::Document(document) => {
            if document.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{ ");
            for (i, (key, value)) in document.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                if key.starts_with(is_identifier_start) && key.chars().all(is_identifier_part) {
                    out.push_str(key);
                } else {
                    out.push_str(&quote(key));
                }
                out.push_str(": ");
                write_shell(out, value);
            }
            out.push_str(" }");
        }
        Bson::Array(values) => {
            if values.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[ ");
            for (i, value) in values.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_shell(out, value);
            }
            out.push_str(" ]");
        }
        Bson::String(text) | Bson::Symbol(text) => out.push_str(&quote(text)),
        Bson::Int32(n) => out.push_str(&n.to_string()),
        Bson::Int64(n) => out.push_str(&format!("NumberLong(\"{n}\")")),
        Bson::Double(n) => out.push_str(&format_double(*n)),
        Bson::Boolean(b) => out.push_str(if *b { "true" } else { "false" }),
        Bson::Null => out.push_str("null"),
        Bson::Undefined => out.push_str("undefined"),
        Bson::ObjectId(id) => out.push_str(&format!("ObjectId(\"{}\")", id.to_hex())),
        Bson::DateTime(date) => match format_date(*date) {
            Some(text) => out.push_str(&format!("ISODate(\"{text}\")")),
            None => out.push_str(&format!("new Date({})", date.timestamp_millis())),
        },
        Bson::Decimal128(n) => out.push_str(&format!("NumberDecimal(\"{n}\")")),
        Bson::Timestamp(t) => out.push_str(&format!("Timestamp({}, {})", t.time, t.increment)),
        Bson::RegularExpression(regex) => {
            out.push('/');
            let mut escaped = false;
            for c in regex.pattern.chars() {
                match c {
                    '/' if !escaped => out.push_str("\\/"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    _ => out.push(c),
                }
                escaped = c == '\\' && !escaped;
            }
            out.push('/');
            // `l` and others JavaScript lacks go to RegExp's string form instead.
            out.push_str(&regex.options);
        }
        Bson::Binary(binary) => match binary.subtype {
            BinarySubtype::Uuid if binary.bytes.len() == 16 => {
                let uuid = binary.to_uuid().expect("16 bytes of subtype 4");
                out.push_str(&format!("UUID(\"{uuid}\")"));
            }
            _ => {
                let json = Bson::Binary(binary.clone()).into_relaxed_extjson();
                let base64 = json["$binary"]["base64"].as_str().unwrap_or_default();
                let subtype: u8 = binary.subtype.into();
                out.push_str(&format!("BinData({subtype}, \"{base64}\")"));
            }
        },
        Bson::MinKey => out.push_str("MinKey()"),
        Bson::MaxKey => out.push_str("MaxKey()"),
        other => out.push_str(&other.clone().into_relaxed_extjson().to_string()),
    }
}

/// A double as JavaScript writes it: `1` for 1.0, `1e+21`, `Infinity`.
pub(crate) fn format_double(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.is_infinite() {
        if n > 0.0 { "Infinity" } else { "-Infinity" }.into()
    } else if n != 0.0 && (n.abs() >= 1e21 || n.abs() < 1e-6) {
        let text = format!("{n:e}");
        match text.split_once('e') {
            Some((mantissa, exponent)) if !exponent.starts_with('-') => {
                format!("{mantissa}e+{exponent}")
            }
            _ => text,
        }
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use mongodb::bson::{doc, oid::ObjectId};

    use super::*;

    /// The one statement in `text`.
    fn one(text: &str) -> Operation {
        let mut statements = parse(text).unwrap_or_else(|err| panic!("{text}: {err:?}"));
        assert_eq!(statements.len(), 1, "{text}");
        statements.remove(0).operation
    }

    /// The value of `db.c.insertOne({ v: <value> })`.
    fn value(value: &str) -> Bson {
        match one(&format!("db.c.insertOne({{ v: {value} }})")) {
            Operation::Collection {
                method: CollectionMethod::InsertOne { document, .. },
                ..
            } => document.get("v").cloned().expect("v"),
            other => panic!("{other:?}"),
        }
    }

    /// The message and position of the error `text` gives.
    fn error(text: &str) -> (String, u32) {
        match parse(text) {
            Err(Error::Database {
                message,
                code,
                position: Some(position),
                ..
            }) if code == "SyntaxError" => (message, position),
            other => panic!("{text}: expected a syntax error, got {other:?}"),
        }
    }

    fn find(operation: Operation) -> (String, Find) {
        match operation {
            Operation::Collection {
                name,
                method: CollectionMethod::Find(find),
            } => (name, find),
            other => panic!("expected find, got {other:?}"),
        }
    }

    #[test]
    fn reads_objects_with_unquoted_and_quoted_keys() {
        let filter = find(one(
            "db.users.find({ age: { $gte: 21, }, 'address.city': \"Seoul\", $or: [{a: 1}, {b: 2},], 3: 'x' })",
        ))
        .1
        .filter;
        assert_eq!(
            filter,
            doc! {
                "age": { "$gte": 21 },
                "address.city": "Seoul",
                "$or": [{ "a": 1 }, { "b": 2 }],
                "3": "x",
            }
        );
    }

    #[test]
    fn reads_strings_with_escapes() {
        assert_eq!(value(r#"'it\'s'"#), Bson::String("it's".into()));
        assert_eq!(
            value(r#""tab\tnew\nquote\" back\\ \x41é\u{1F600}😀""#),
            Bson::String("tab\tnew\nquote\" back\\ Aé😀😀".into())
        );
        assert_eq!(value("'한글'"), Bson::String("한글".into()));
    }

    #[test]
    fn reads_numbers_as_int32_int64_or_double() {
        assert_eq!(value("42"), Bson::Int32(42));
        assert_eq!(value("-7"), Bson::Int32(-7));
        assert_eq!(value("2147483648"), Bson::Int64(2_147_483_648));
        assert_eq!(value("-9223372036854775808"), Bson::Int64(i64::MIN));
        assert_eq!(value("99999999999999999999"), Bson::Double(1e20));
        assert_eq!(value("1.5"), Bson::Double(1.5));
        assert_eq!(value("1e3"), Bson::Double(1000.0));
        assert_eq!(value("-2.5E-1"), Bson::Double(-0.25));
        assert_eq!(value(".5"), Bson::Double(0.5));
        assert_eq!(value("0x1F"), Bson::Int32(31));
        assert_eq!(value("-Infinity"), Bson::Double(f64::NEG_INFINITY));
        assert!(matches!(value("NaN"), Bson::Double(n) if n.is_nan()));
    }

    #[test]
    fn reads_literals_and_nesting() {
        assert_eq!(value("true"), Bson::Boolean(true));
        assert_eq!(value("false"), Bson::Boolean(false));
        assert_eq!(value("null"), Bson::Null);
        assert_eq!(value("undefined"), Bson::Null);
        assert_eq!(
            value("[1, [2, { a: [] }], {}]"),
            Bson::Array(vec![
                Bson::Int32(1),
                Bson::Array(vec![Bson::Int32(2), Bson::Document(doc! { "a": [] })]),
                Bson::Document(doc! {}),
            ])
        );
    }

    #[test]
    fn reads_regex_literals() {
        assert_eq!(
            value(r"/^a\/b[/]c/mi"),
            Bson::RegularExpression(Regex {
                pattern: r"^a\/b[/]c".into(),
                options: "im".into(),
            })
        );
        assert_eq!(
            value(r#"RegExp("x.y", "s")"#),
            Bson::RegularExpression(Regex {
                pattern: "x.y".into(),
                options: "s".into(),
            })
        );
        assert_eq!(error("db.c.find({ a: /x/g })").1, 19);
    }

    #[test]
    fn reads_bson_constructors() {
        let id = "507f1f77bcf86cd799439011";
        assert_eq!(
            value(&format!("ObjectId('{id}')")),
            Bson::ObjectId(ObjectId::parse_str(id).unwrap())
        );
        assert_eq!(
            value(&format!("new ObjectId(\"{id}\")")),
            value(&format!("ObjectId(\"{id}\")"))
        );
        assert!(matches!(value("ObjectId()"), Bson::ObjectId(_)));

        let date = Bson::DateTime(DateTime::from_millis(1_714_557_600_000));
        assert_eq!(value("ISODate('2024-05-01T10:00:00Z')"), date);
        assert_eq!(value("new ISODate('2024-05-01T10:00:00.000Z')"), date);
        assert_eq!(value("new Date('2024-05-01T19:00:00+09:00')"), date);
        assert_eq!(value("new Date(1714557600000)"), date);
        assert_eq!(
            value("ISODate('2024-05-01')"),
            Bson::DateTime(DateTime::from_millis(1_714_521_600_000))
        );
        assert!(matches!(value("new Date()"), Bson::DateTime(_)));
        assert!(matches!(value("Date()"), Bson::DateTime(_)));

        assert_eq!(value("NumberInt(5)"), Bson::Int32(5));
        assert_eq!(value("NumberInt('5')"), Bson::Int32(5));
        assert_eq!(value("NumberLong(5)"), Bson::Int64(5));
        assert_eq!(
            value("NumberLong('9223372036854775807')"),
            Bson::Int64(i64::MAX)
        );
        assert_eq!(
            value("NumberDecimal('1.10')"),
            Bson::Decimal128("1.10".parse().unwrap())
        );
        assert_eq!(value("Decimal128('2')"), value("new NumberDecimal(\"2\")"));
        let uuid = "01234567-89ab-cdef-0123-456789abcdef";
        assert_eq!(
            value(&format!("UUID('{uuid}')")),
            Bson::Binary(Binary::from_uuid(bson::Uuid::parse_str(uuid).unwrap()))
        );
        assert_eq!(
            value("Timestamp(1700000000, 3)"),
            Bson::Timestamp(Timestamp {
                time: 1_700_000_000,
                increment: 3
            })
        );
        assert_eq!(
            value("Timestamp({ t: 1700000000, i: 3 })"),
            value("Timestamp(1700000000, 3)")
        );
        assert_eq!(
            value("BinData(0, 'aGVsbG8=')"),
            Bson::Binary(Binary {
                subtype: BinarySubtype::Generic,
                bytes: b"hello".to_vec()
            })
        );
        assert_eq!(value("MinKey()"), Bson::MinKey);
        assert_eq!(value("MaxKey"), Bson::MaxKey);
    }

    #[test]
    fn points_at_mistakes_by_character() {
        assert_eq!(
            error("db.users.find({ a: })"),
            ("expected a value, found '}'".into(), 20)
        );
        // Characters, not bytes: "é" is two bytes.
        assert_eq!(error("db.c.find({ é: 'x' 1 })").1, 20);
        assert_eq!(error("db.c.find({ a: 'open })").1, 16);
        assert_eq!(error("db.c.find({ a: ObjectId('xyz') })").1, 25);
        assert_eq!(error("db.c.find({ a: ISODate('May 1') })").1, 24);
        assert_eq!(error("db.c.find({ a: foo })").1, 16);
        assert_eq!(error("db.c.find({ a: 1 }").1, 19);
        assert_eq!(error("select * from t").1, 1);
        assert_eq!(error("db.c.frobnicate()").1, 6);
        assert_eq!(error("db.c.find().sort()").1, 18);
        assert_eq!(error("db.c.insertOne(1)").1, 16);
        assert_eq!(error("db.c.updateOne({}, {}, { upsert: 'yes' })").1, 24);
        assert_eq!(error("db.c.find() db.d.find()").1, 13);
        assert_eq!(error("db.c.find() /* open").1, 13);
        assert_eq!(error("show users").1, 6);
        assert_eq!(error("show   users").1, 8);
        assert_eq!(error("use \n").1, 5);
    }

    #[test]
    fn splits_statements_at_semicolons_and_line_breaks() {
        let text = "use shop; show collections\n\
                    db.users.find({\n  a: 1\n})\n\
                    // a comment; with a semicolon\n\
                    db.users.countDocuments() /* done */ ;;\n\
                    show dbs";
        let statements = parse(text).unwrap();
        let ranges: Vec<_> = statements.iter().map(|s| &text[s.range.clone()]).collect();
        assert_eq!(
            ranges,
            [
                "use shop",
                "show collections",
                "db.users.find({\n  a: 1\n})",
                "db.users.countDocuments()",
                "show dbs",
            ]
        );
        assert!(matches!(&statements[0].operation, Operation::Use(name) if name == "shop"));
        assert!(matches!(
            statements[1].operation,
            Operation::Database(DatabaseMethod::GetCollectionNames)
        ));
        assert!(matches!(statements[4].operation, Operation::ShowDatabases));
    }

    #[test]
    fn reads_chained_find_calls() {
        let (name, chained) = find(one(
            "db.orders.find({ total: { $gt: 10 } }, { _id: 0 })\n  .sort({ total: -1 })\n  // newest first\n  .skip(5).limit(20)\n  .toArray()",
        ));
        assert_eq!(name, "orders");
        assert_eq!(
            chained,
            Find {
                filter: doc! { "total": { "$gt": 10 } },
                projection: Some(doc! { "_id": 0 }),
                sort: Some(doc! { "total": -1 }),
                limit: Some(20),
                skip: Some(5),
                count: false,
            }
        );
        let (_, counted) = find(one("db.orders.find().projection({ a: 1 }).count()"));
        assert!(counted.count);
        assert_eq!(counted.projection, Some(doc! { "a": 1 }));
        assert_eq!(error("db.c.find().count().limit(1)").1, 21);
    }

    #[test]
    fn names_collections_with_dots_brackets_and_get_collection() {
        assert_eq!(find(one("db.system.views.find()")).0, "system.views");
        assert_eq!(find(one("db['my-coll'].find()")).0, "my-coll");
        assert_eq!(find(one("db.getCollection(\"a b\").find()")).0, "a b");
    }

    #[test]
    fn reads_get_sibling_db_as_the_statements_database() {
        let statement = |text: &str| parse(text).unwrap().remove(0);

        let shown =
            statement("db.getSiblingDB(\"shop\").getCollection(\"orders\").find({}).limit(1000)");
        assert_eq!(shown.database.as_deref(), Some("shop"));
        let (name, find) = find(shown.operation);
        assert_eq!((name.as_str(), find.limit), ("orders", Some(1000)));

        let counted = statement("db.getSiblingDB('shop').orders.countDocuments({})");
        assert_eq!(counted.database.as_deref(), Some("shop"));
        assert!(matches!(
            counted.operation,
            Operation::Collection { name, method: CollectionMethod::CountDocuments { .. } } if name == "orders"
        ));

        let command = statement("db.getSiblingDB(\"shop\").runCommand({ ping: 1 })");
        assert_eq!(command.database.as_deref(), Some("shop"));
        assert!(matches!(
            command.operation,
            Operation::Database(DatabaseMethod::RunCommand { admin: false, .. })
        ));

        let dropped = statement("db.getSiblingDB(\"shop\").dropDatabase()");
        assert_eq!(dropped.database.as_deref(), Some("shop"));
        assert_eq!(dropped.operation.write_method(), Some("dropDatabase"));

        assert_eq!(statement("db.logs.find()").database, None);
        assert_eq!(error("db.getSiblingDB(1).c.find()").1, 17);
        assert_eq!(error("db.getSiblingDB('shop')").1, 24);
    }

    #[test]
    fn reads_database_methods() {
        assert!(matches!(
            one("db.runCommand({ ping: 1 })"),
            Operation::Database(DatabaseMethod::RunCommand { command, admin: false })
                if command == doc! { "ping": 1 }
        ));
        assert!(matches!(
            one("db.adminCommand('listDatabases')"),
            Operation::Database(DatabaseMethod::RunCommand { command, admin: true })
                if command == doc! { "listDatabases": 1 }
        ));
        assert!(matches!(
            one("db.createView('adults', 'people', [{ $match: { age: { $gte: 18 } } }])"),
            Operation::Database(DatabaseMethod::CreateView { name, source, pipeline, .. })
                if name == "adults" && source == "people" && pipeline.len() == 1
        ));
        assert!(matches!(
            one("db.createCollection('logs', { capped: true, size: 4096 })"),
            Operation::Database(DatabaseMethod::CreateCollection { options, .. })
                if options == doc! { "capped": true, "size": 4096 }
        ));
        assert!(matches!(
            one("db.stats()"),
            Operation::Database(DatabaseMethod::Stats)
        ));
        assert!(matches!(
            one("db.dropDatabase()"),
            Operation::Database(DatabaseMethod::DropDatabase)
        ));
        assert!(matches!(
            one("db.getName()"),
            Operation::Database(DatabaseMethod::GetName)
        ));
    }

    #[test]
    fn reads_collection_methods() {
        let method = |text: &str| match one(text) {
            Operation::Collection { method, .. } => method,
            other => panic!("{other:?}"),
        };
        assert!(matches!(
            method("db.c.updateMany({}, [{ $set: { a: 1 } }], { upsert: true })"),
            CollectionMethod::Update { update: Update::Pipeline(p), options: Some(o), many: true, .. }
                if p.len() == 1 && o.upsert == Some(true)
        ));
        assert!(matches!(
            method("db.c.aggregate({ $match: {} }, { $count: 'n' })"),
            CollectionMethod::Aggregate { pipeline, .. } if pipeline.len() == 2
        ));
        assert!(matches!(
            method("db.c.aggregate([{ $match: {} }], { allowDiskUse: true }).toArray()"),
            CollectionMethod::Aggregate { options: Some(o), .. } if o.allow_disk_use == Some(true)
        ));
        assert!(matches!(
            method("db.c.distinct('tags', { a: 1 })"),
            CollectionMethod::Distinct { field, filter, .. } if field == "tags" && filter == doc! { "a": 1 }
        ));
        assert!(matches!(
            method("db.c.dropIndex({ a: 1 })"),
            CollectionMethod::DropIndex(Bson::Document(_))
        ));
        assert!(matches!(
            method("db.c.renameCollection('d', true)"),
            CollectionMethod::RenameCollection { to, drop_target: true } if to == "d"
        ));
        assert!(matches!(
            method("db.c.getIndexes()"),
            CollectionMethod::GetIndexes
        ));
        assert!(matches!(
            method("db.c.estimatedDocumentCount()"),
            CollectionMethod::EstimatedDocumentCount(None)
        ));
    }

    #[test]
    fn statement_ranges_follow_execute() {
        let ranges = |text: &str| -> Vec<String> {
            statement_ranges(text)
                .into_iter()
                .map(|r| text[r].to_owned())
                .collect()
        };
        // Multi-line statements, comments between statements, and a last
        // statement with no line break after it.
        let text = "  db.a.find({\n    x: 1, // inner comment\n  })\n  .limit(2);\n\n/* between */\n// line\ndb.b.insertOne({ s: ';' })\nshow dbs";
        assert_eq!(
            ranges(text),
            [
                "db.a.find({\n    x: 1, // inner comment\n  })\n  .limit(2)",
                "db.b.insertOne({ s: ';' })",
                "show dbs",
            ]
        );
        // Each range runs as exactly that statement.
        for range in statement_ranges(text) {
            assert_eq!(parse(&text[range]).unwrap().len(), 1);
        }

        assert_eq!(ranges(""), Vec::<String>::new());
        assert_eq!(ranges("  // only a comment\n"), Vec::<String>::new());
        assert_eq!(ranges("use a;use b"), ["use a", "use b"]);

        // Past a mistake, the rest is one range that fails the same way.
        let text = "db.a.find()\ndb.b.find({ x: })\ndb.c.find()\n";
        assert_eq!(
            ranges(text),
            ["db.a.find()", "db.b.find({ x: })\ndb.c.find()"]
        );
        assert_eq!(ranges("db.a.find() /* open"), ["db.a.find() /* open"]);
        assert_eq!(ranges("show dbs\n/* open"), ["show dbs", "/* open"]);
    }

    #[test]
    fn reads_and_writes_dates() {
        let date = parse_date("2024-02-29T23:59:59.5Z").unwrap();
        assert_eq!(format_date(date).unwrap(), "2024-02-29T23:59:59.500Z");
        assert_eq!(
            format_date(parse_date("1969-12-31 23:00").unwrap()).unwrap(),
            "1969-12-31T23:00:00.000Z"
        );
        assert_eq!(parse_date("20240501"), parse_date("2024-05-01"));
        assert_eq!(parse_date("2023-02-29"), None);
        assert_eq!(parse_date("2024-05-01T24:00"), None);
        assert_eq!(parse_date("May 1, 2024"), None);
        assert_eq!(format_date(DateTime::from_millis(i64::MAX)), None);
    }

    #[test]
    fn writes_values_mongosh_reads_back() {
        let value = Bson::Document(doc! {
            "email": 1,
            "a.b": -1,
            "$match": { "when": DateTime::from_millis(0), "id": ObjectId::parse_str("507f1f77bcf86cd799439011").unwrap() },
            "big": 5_i64,
            "re": Regex { pattern: r"a\/b".into(), options: "i".into() },
            "list": [1.5, "x", null, true],
            "empty": {},
        });
        let text = to_shell(&value);
        assert_eq!(
            text,
            "{ email: 1, \"a.b\": -1, $match: { when: ISODate(\"1970-01-01T00:00:00.000Z\"), \
             id: ObjectId(\"507f1f77bcf86cd799439011\") }, big: NumberLong(\"5\"), re: /a\\/b/i, \
             list: [ 1.5, \"x\", null, true ], empty: {} }"
        );
        let Operation::Collection {
            method: CollectionMethod::InsertOne { document, .. },
            ..
        } = one(&format!("db.c.insertOne({text})"))
        else {
            panic!("expected insertOne");
        };
        assert_eq!(Bson::Document(document), value);
        // A slash the pattern doesn't escape is escaped, to end the literal right.
        let regex = Regex {
            pattern: "a/b".into(),
            options: "i".into(),
        };
        assert_eq!(to_shell(&Bson::RegularExpression(regex)), r"/a\/b/i");
        assert_eq!(format_double(1.0), "1");
        assert_eq!(format_double(1e21), "1e+21");
        assert_eq!(format_double(1e-7), "1e-7");
    }
}
