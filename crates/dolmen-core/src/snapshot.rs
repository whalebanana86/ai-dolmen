//! Schema snapshots: a database's structure as one DDL file per object, kept in Git.
//!
//! Layout inside the snapshot folder:
//!
//! ```text
//! extensions.sql
//! <schema>/schema.sql             CREATE SCHEMA (not for public, which every database has)
//! <schema>/tables/<table>.sql     with its constraints, indexes and comments
//! <schema>/views/<view>.sql
//! <schema>/functions/<name>.sql   every overload of the name
//! <schema>/sequences/<name>.sql
//! <schema>/types/<name>.sql
//! ```
//!
//! The folder holds a `.dolmen-snapshot` marker. Dolmen only writes into empty
//! folders or folders with the marker, so it never deletes files it doesn't own.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::Serialize;
use similar::TextDiff;

use crate::Error;
use crate::postgres::ddl;
use crate::schema::Database;

pub const MARKER: &str = ".dolmen-snapshot";
const MARKER_TEXT: &str = "This folder is a Dolmen schema snapshot. Its .sql files are regenerated \
from the database, so change the database rather than editing them.\n";

/// Snapshot files keyed by their path relative to the snapshot folder, with `/` separators.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    files: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub path: String,
    pub kind: ChangeKind,
    /// The change as a unified diff.
    pub diff: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeKind {
    Added,
    Modified,
    Removed,
}

impl Snapshot {
    pub fn of(database: &Database) -> Self {
        let mut files = BTreeMap::new();
        if !database.extensions.is_empty() {
            let statements: String = database
                .extensions
                .iter()
                .map(ddl::create_extension)
                .collect();
            files.insert("extensions.sql".to_owned(), statements);
        }

        for schema in &database.schemas {
            let dir = file_name(&schema.name);
            if schema.name != "public" {
                files.insert(
                    format!("{dir}/schema.sql"),
                    ddl::create_schema(&schema.name),
                );
            }
            for table in &schema.tables {
                files.insert(
                    format!("{dir}/tables/{}.sql", file_name(&table.name)),
                    ddl::create_table(&schema.name, table),
                );
            }
            for view in &schema.views {
                files.insert(
                    format!("{dir}/views/{}.sql", file_name(&view.name)),
                    ddl::create_view(&schema.name, view),
                );
            }
            // Overloads share a name and a file, one statement after another.
            for function in &schema.functions {
                let file = files
                    .entry(format!("{dir}/functions/{}.sql", file_name(&function.name)))
                    .or_default();
                if !file.is_empty() {
                    file.push('\n');
                }
                file.push_str(&ddl::create_function(function));
            }
            for sequence in &schema.sequences {
                files.insert(
                    format!("{dir}/sequences/{}.sql", file_name(&sequence.name)),
                    ddl::create_sequence(&schema.name, sequence),
                );
            }
            for enum_type in &schema.enums {
                files.insert(
                    format!("{dir}/types/{}.sql", file_name(&enum_type.name)),
                    ddl::create_enum(&schema.name, enum_type),
                );
            }
        }
        Self { files }
    }

    pub fn files(&self) -> &BTreeMap<String, String> {
        &self.files
    }

    /// Reads the snapshot in `dir`. A missing folder is an empty snapshot.
    pub fn read(dir: &Path) -> Result<Self, Error> {
        let mut files = BTreeMap::new();
        for path in sql_files(dir)? {
            let content = fs::read_to_string(&path).map_err(|err| io_error(&path, err))?;
            files.insert(relative(dir, &path), content);
        }
        Ok(Self { files })
    }

    /// What changes from this snapshot to `newer`, sorted by path.
    pub fn changes(&self, newer: &Snapshot) -> Vec<FileChange> {
        let paths: std::collections::BTreeSet<&String> =
            self.files.keys().chain(newer.files.keys()).collect();
        paths
            .into_iter()
            .filter_map(|path| {
                let old = self.files.get(path);
                let new = newer.files.get(path);
                let kind = match (old, new) {
                    (None, Some(_)) => ChangeKind::Added,
                    (Some(_), None) => ChangeKind::Removed,
                    (Some(old), Some(new)) if old != new => ChangeKind::Modified,
                    _ => return None,
                };
                let diff = TextDiff::from_lines(
                    old.map_or("", String::as_str),
                    new.map_or("", String::as_str),
                )
                .unified_diff()
                .context_radius(3)
                .header(&format!("a/{path}"), &format!("b/{path}"))
                .to_string();
                Some(FileChange {
                    path: path.clone(),
                    kind,
                    diff,
                })
            })
            .collect()
    }

    /// Makes `dir` hold exactly this snapshot: writes new and changed files,
    /// removes files of objects that no longer exist, and leaves the rest alone.
    pub fn write(&self, dir: &Path) -> Result<(), Error> {
        let marker = dir.join(MARKER);
        if !marker.exists() && !is_empty_or_missing(dir)? {
            return Err(Error::Storage {
                message: format!(
                    "{} isn't empty and isn't a Dolmen snapshot folder; choose an empty folder",
                    dir.display()
                ),
            });
        }
        fs::create_dir_all(dir).map_err(|err| io_error(dir, err))?;
        if !marker.exists() {
            fs::write(&marker, MARKER_TEXT).map_err(|err| io_error(&marker, err))?;
        }

        let existing = Snapshot::read(dir)?;
        for (path, content) in &self.files {
            if existing.files.get(path) == Some(content) {
                continue;
            }
            let file = dir.join(path);
            if let Some(parent) = file.parent() {
                fs::create_dir_all(parent).map_err(|err| io_error(parent, err))?;
            }
            fs::write(&file, content).map_err(|err| io_error(&file, err))?;
        }
        for path in existing
            .files
            .keys()
            .filter(|path| !self.files.contains_key(*path))
        {
            let file = dir.join(path);
            fs::remove_file(&file).map_err(|err| io_error(&file, err))?;
        }
        remove_empty_dirs(dir)?;
        Ok(())
    }
}

/// A file name for an object name. Lower-case ASCII letters, digits, `_`, `-`
/// and non-ASCII characters are kept; everything else is percent-encoded, so
/// names never clash on case-insensitive file systems or escape the folder.
pub fn file_name(name: &str) -> String {
    let mut encoded = String::with_capacity(name.len());
    for (i, ch) in name.char_indices() {
        let keep = ch.is_ascii_lowercase()
            || ch.is_ascii_digit()
            || ch == '_'
            || ch == '-'
            || (ch == '.' && i > 0)
            || !ch.is_ascii();
        if keep {
            encoded.push(ch);
        } else {
            encoded.push_str(&format!("%{:02X}", ch as u32));
        }
    }
    encoded
}

fn sql_files(dir: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut files = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(files),
        Err(err) => return Err(io_error(dir, err)),
    };
    for entry in entries {
        let path = entry.map_err(|err| io_error(dir, err))?.path();
        if path.is_dir() {
            files.extend(sql_files(&path)?);
        } else if path.extension().is_some_and(|ext| ext == "sql") {
            files.push(path);
        }
    }
    Ok(files)
}

fn relative(dir: &Path, path: &Path) -> String {
    path.strip_prefix(dir)
        .unwrap_or(path)
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn is_empty_or_missing(dir: &Path) -> Result<bool, Error> {
    match fs::read_dir(dir) {
        // Finder's metadata file doesn't make a folder "used".
        Ok(mut entries) => {
            Ok(entries.all(|entry| entry.is_ok_and(|e| e.file_name() == ".DS_Store")))
        }
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(true),
        Err(err) => Err(io_error(dir, err)),
    }
}

/// Removes folders under `dir` (not `dir` itself) that have nothing left in them.
fn remove_empty_dirs(dir: &Path) -> Result<(), Error> {
    for entry in fs::read_dir(dir).map_err(|err| io_error(dir, err))? {
        let path = entry.map_err(|err| io_error(dir, err))?.path();
        if path.is_dir() {
            remove_empty_dirs(&path)?;
            if fs::read_dir(&path)
                .map_err(|err| io_error(&path, err))?
                .next()
                .is_none()
            {
                fs::remove_dir(&path).map_err(|err| io_error(&path, err))?;
            }
        }
    }
    Ok(())
}

fn io_error(path: &Path, err: std::io::Error) -> Error {
    Error::Storage {
        message: format!("{}: {err}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_file_names_that_could_clash_or_escape() {
        assert_eq!(file_name("users"), "users");
        assert_eq!(file_name("order_items-v2"), "order_items-v2");
        assert_eq!(file_name("Users"), "%55sers");
        assert_eq!(file_name("a/b"), "a%2Fb");
        assert_eq!(file_name(".."), "%2E.");
        assert_eq!(file_name("사람"), "사람");
    }
}
