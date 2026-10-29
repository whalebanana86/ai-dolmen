//! Schema snapshots: a database's structure as one DDL file per object, kept in Git.
//!
//! Layout inside the snapshot folder:
//!
//! ```text
//! extensions.sql
//! <schema>/schema.sql             CREATE SCHEMA (not for public, which every database has)
//! <schema>/tables/<table>.sql     with its constraints, indexes, triggers, policies and comments
//! <schema>/views/<view>.sql
//! <schema>/functions/<name>.sql   every overload of the name
//! <schema>/sequences/<name>.sql
//! <schema>/types/<name>.sql
//! ```
//!
//! Next to the DDL, `dolmen-model.json` holds the schema model the files were
//! written from, so a past snapshot can be applied to a database exactly,
//! without parsing SQL.
//!
//! The folder holds a `.dolmen-snapshot` marker. Dolmen only writes into empty
//! folders or folders with the marker, so it never deletes files it doesn't own.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use similar::TextDiff;

use crate::Error;
use crate::postgres::ddl;
use crate::schema::Database;

pub const MARKER: &str = ".dolmen-snapshot";
pub const MODEL_FILE: &str = "dolmen-model.json";
const MODEL_FORMAT: u32 = 1;
/// Marks the model as generated, so GitHub collapses it in pull request diffs.
const GITATTRIBUTES: &str = "dolmen-model.json linguist-generated=true\n";
const MARKER_TEXT: &str = "This folder is a Dolmen schema snapshot. Its .sql files are regenerated \
from the database, so change the database rather than editing them.\n";

/// Snapshot files keyed by their path relative to the snapshot folder, with
/// `/` separators, and the model they were written from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    files: BTreeMap<String, String>,
    /// `None` for snapshots written before Dolmen saved models.
    model: Option<Database>,
}

#[derive(Serialize, Deserialize)]
struct ModelFile {
    format: u32,
    database: Database,
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
        Self {
            files,
            model: Some(database.clone()),
        }
    }

    /// A snapshot from files read elsewhere, such as from a Git commit, with
    /// the contents of its model file if it has one.
    pub fn from_files(
        files: BTreeMap<String, String>,
        model_json: Option<&str>,
    ) -> Result<Self, Error> {
        let model = model_json
            .map(|json| parse_model(json).map_err(|err| storage(format!("{MODEL_FILE}: {err}"))))
            .transpose()?;
        Ok(Self { files, model })
    }

    pub fn files(&self) -> &BTreeMap<String, String> {
        &self.files
    }

    /// The schema model the snapshot was written from.
    pub fn model(&self) -> Option<&Database> {
        self.model.as_ref()
    }

    /// Reads the snapshot in `dir`. A missing folder is an empty snapshot.
    pub fn read(dir: &Path) -> Result<Self, Error> {
        let mut files = BTreeMap::new();
        for path in sql_files(dir)? {
            let content = fs::read_to_string(&path).map_err(|err| io_error(&path, err))?;
            files.insert(relative(dir, &path), content);
        }
        let model_path = dir.join(MODEL_FILE);
        refuse_link(&model_path)?;
        let model_json = match fs::read_to_string(&model_path) {
            Ok(json) => Some(json),
            Err(err) if err.kind() == ErrorKind::NotFound => None,
            Err(err) => return Err(io_error(&model_path, err)),
        };
        Self::from_files(files, model_json.as_deref())
    }

    /// What changes in the DDL files from this snapshot to `newer`, sorted by path.
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
        for file in [&marker, &dir.join(".gitattributes"), &dir.join(MODEL_FILE)] {
            refuse_link(file)?;
        }
        if !marker.exists() {
            fs::write(&marker, MARKER_TEXT).map_err(|err| io_error(&marker, err))?;
            let attributes = dir.join(".gitattributes");
            if !attributes.exists() {
                fs::write(&attributes, GITATTRIBUTES).map_err(|err| io_error(&attributes, err))?;
            }
        }
        if let Some(database) = &self.model {
            write_if_changed(&dir.join(MODEL_FILE), &model_json(database))?;
        }

        // Reading refuses links anywhere in the folder, so the writes and
        // removals below stay inside it.
        let existing = Snapshot::read(dir)?;
        for (path, content) in &self.files {
            let file = dir.join(path);
            refuse_links_below(dir, &file)?;
            write_if_changed(&file, content)?;
        }
        for path in existing
            .files
            .keys()
            .filter(|path| !self.files.contains_key(*path))
        {
            let file = dir.join(path);
            refuse_links_below(dir, &file)?;
            fs::remove_file(&file).map_err(|err| io_error(&file, err))?;
        }
        remove_empty_dirs(dir)?;
        Ok(())
    }
}

/// The model file's contents: pretty-printed, so Git can diff it line by line.
fn model_json(database: &Database) -> String {
    let file = ModelFile {
        format: MODEL_FORMAT,
        database: database.clone(),
    };
    let mut json = serde_json::to_string_pretty(&file).expect("the model serializes");
    json.push('\n');
    json
}

fn parse_model(json: &str) -> Result<Database, String> {
    let file: ModelFile = serde_json::from_str(json).map_err(|err| err.to_string())?;
    if file.format != MODEL_FORMAT {
        return Err(format!(
            "format {} needs a newer Dolmen (this one reads format {MODEL_FORMAT})",
            file.format
        ));
    }
    Ok(file.database)
}

/// Writes `content` unless the file already holds it, creating folders as needed.
fn write_if_changed(file: &Path, content: &str) -> Result<(), Error> {
    if fs::read_to_string(file).is_ok_and(|existing| existing == content) {
        return Ok(());
    }
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).map_err(|err| io_error(parent, err))?;
    }
    fs::write(file, content).map_err(|err| io_error(file, err))
}

fn storage(message: String) -> Error {
    Error::Storage { message }
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

/// The `.sql` files under `dir`. Hidden folders, such as a `.git` of a
/// repository made right in the folder, are left alone, and links are refused.
fn sql_files(dir: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut files = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(files),
        Err(err) => return Err(io_error(dir, err)),
    };
    for entry in entries {
        let entry = entry.map_err(|err| io_error(dir, err))?;
        let path = entry.path();
        // file_type doesn't follow links, unlike Path::is_dir.
        let kind = entry.file_type().map_err(|err| io_error(&path, err))?;
        if kind.is_symlink() {
            return Err(link_error(&path));
        } else if kind.is_dir() {
            if !is_hidden(&path) {
                files.extend(sql_files(&path)?);
            }
        } else if path.extension().is_some_and(|ext| ext == "sql") {
            files.push(path);
        }
    }
    Ok(files)
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with('.'))
}

/// Snapshot folders hold plain files and folders. A symbolic link, say in a
/// cloned repository, could send writes and removals outside the folder.
fn refuse_link(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(link_error(path)),
        _ => Ok(()),
    }
}

/// Refuses `path` when it, or a folder between `dir` and it, is a link.
fn refuse_links_below(dir: &Path, path: &Path) -> Result<(), Error> {
    for ancestor in path.ancestors().take_while(|a| *a != dir) {
        refuse_link(ancestor)?;
    }
    Ok(())
}

fn link_error(path: &Path) -> Error {
    Error::Storage {
        message: format!(
            "{} is a symbolic link; a snapshot folder may only hold plain files and folders",
            path.display()
        ),
    }
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

/// Removes folders under `dir` (not `dir` itself) that have nothing left in
/// them, leaving hidden folders and links alone.
fn remove_empty_dirs(dir: &Path) -> Result<(), Error> {
    for entry in fs::read_dir(dir).map_err(|err| io_error(dir, err))? {
        let entry = entry.map_err(|err| io_error(dir, err))?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|err| io_error(&path, err))?;
        if kind.is_dir() && !is_hidden(&path) {
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
