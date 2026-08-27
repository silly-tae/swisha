//! Reading configuration from the process environment or a `KEY=value` file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use crate::error::{Context, Result, err};

/// Configuration lookup: the process environment first, then an optional file.
///
/// The file becomes a map and is never written back. `env::set_var` is unsafe in edition 2024
/// because POSIX `setenv` races other threads, and a runtime is already running by then.
#[derive(Debug)]
pub struct Env {
    file: Option<EnvFile>,
}

impl Env {
    /// Reads the process environment only.
    pub fn from_process() -> Self {
        Self { file: None }
    }

    /// Loads the file named by `SWISHA_ENV_FILE`, if that variable is set.
    ///
    /// A named file that cannot be read is an error rather than a silent fallback: a service
    /// told to load its configuration from somewhere should not start with a different one.
    pub fn discover() -> Result<Self> {
        match std::env::var("SWISHA_ENV_FILE") {
            Ok(path) if !path.trim().is_empty() => Ok(Self {
                file: Some(EnvFile::load(Path::new(path.trim()))?),
            }),
            _ => Ok(Self::from_process()),
        }
    }

    /// Uses an already-loaded file.
    pub fn with_file(file: EnvFile) -> Self {
        Self { file: Some(file) }
    }

    /// Looks a key up. The process environment wins, so a systemd unit or `docker --env-file`
    /// can override a value without the file being edited.
    pub fn get(&self, key: &str) -> Option<String> {
        match std::env::var(key) {
            Ok(value) => Some(value),
            Err(_) => self.file.as_ref()?.get(key).map(str::to_string),
        }
    }

    /// A setting that must be present and non-blank, naming the file in the error when one is
    /// loaded.
    pub fn required(&self, key: &str) -> Result<String> {
        self.get(key)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| match self.source() {
                Some(path) => err(format!(
                    "Missing required setting {key}: not in the environment, and not in {}",
                    path.display()
                )),
                None => err(format!("Missing required env var: {key}")),
            })
    }

    /// A setting with a fallback.
    ///
    /// A key that is present but blank reads as not configured, exactly as
    /// [`required`](Self::required) treats it. That is what lets a template ship its fields
    /// empty: without it a blank line would silently replace the default.
    pub fn optional(&self, key: &str, default: &str) -> String {
        self.get(key)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| default.to_string())
    }

    /// Where a file-supplied value came from, so a misconfiguration can name the file.
    pub fn source(&self) -> Option<&Path> {
        self.file.as_ref().map(|file| file.path.as_path())
    }
}

/// A parsed `KEY=value` file.
#[derive(Debug)]
pub struct EnvFile {
    path: PathBuf,
    values: HashMap<String, String>,
}

impl EnvFile {
    /// Reads and parses a file.
    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("Cannot read env file: {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            values: parse(&contents, path)?,
        })
    }

    /// One value from the file, ignoring the process environment.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// How many settings the file carries.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether the file carries no settings at all. A file of nothing but comments is still a
    /// loaded file.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// Parses `KEY=value` lines.
///
/// Accepts an `export ` prefix, skips blanks and `#` comments, strips matching quotes without
/// interpreting escapes, and cuts an unquoted value at a ` #` comment. A duplicate key is an
/// error: silently taking one of two is how a service ends up running configuration nobody
/// intended.
pub fn parse(contents: &str, path: &Path) -> Result<HashMap<String, String>> {
    let mut values = HashMap::new();

    for (index, raw) in contents.lines().enumerate() {
        let number = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, value)) = line.split_once('=') else {
            return Err(err(format!(
                "{}:{number}: expected KEY=value",
                path.display()
            )));
        };

        let key = key.trim();
        if !is_valid_key(key) {
            return Err(err(format!(
                "{}:{number}: '{key}' is not a valid variable name",
                path.display()
            )));
        }

        // A duplicate is almost always a mistake, and silently taking one of the two is how a
        // service ends up running with configuration nobody intended.
        if values.contains_key(key) {
            return Err(err(format!(
                "{}:{number}: '{key}' is set more than once",
                path.display()
            )));
        }

        values.insert(key.to_string(), unquote(value).to_string());
    }

    Ok(values)
}

fn is_valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// Quotes are stripped verbatim with no escape processing, so a value containing a backslash
// means what it says. An unquoted value is cut at a ` #` comment; quote the value to keep one.
// The cut happens before trimming, so a field left blank with a trailing comment reads as blank
// rather than as the comment itself.
fn unquote(value: &str) -> &str {
    let trimmed = value.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = trimmed.strip_prefix(quote)
            && let Some(inner) = inner.strip_suffix(quote)
        {
            return inner;
        }
    }
    match value.split_once(" #") {
        Some((before, _)) => before.trim(),
        None => trimmed,
    }
}
