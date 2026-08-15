//! Byte-level inventory for the authored Dig and Pet asset trees.
//!
//! The manifest is generated from the repository's ``assets/dig`` and
//! ``assets/pets`` roots.  Those roots are copied to the same relative paths
//! below ``/app/assets`` by the Rust image, so this verifier can be used both
//! against a checkout and against an extracted runtime image.

use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const MANIFEST_JSON: &str = include_str!("authored_asset_manifest.json");

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct ManifestEntry {
    pub path: String,
    pub bytes: usize,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
struct ManifestDocument {
    schema: u8,
    files: Vec<ManifestEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManifestError {
    Parse(String),
    UnsupportedSchema(u8),
    DuplicatePath(String),
    InvalidPath(String),
    MissingFile(PathBuf),
    Read {
        path: PathBuf,
        error: String,
    },
    SizeMismatch {
        path: PathBuf,
        expected: usize,
        actual: usize,
    },
    HashMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    UnmanifestedFile(PathBuf),
    MissingManifestEntry(PathBuf),
}

impl Display for ManifestError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(error) => write!(formatter, "invalid authored asset manifest: {error}"),
            Self::UnsupportedSchema(schema) => {
                write!(
                    formatter,
                    "unsupported authored asset manifest schema {schema}"
                )
            }
            Self::DuplicatePath(path) => write!(formatter, "duplicate manifest path {path}"),
            Self::InvalidPath(path) => write!(formatter, "invalid manifest path {path}"),
            Self::MissingFile(path) => {
                write!(formatter, "missing authored asset {}", path.display())
            }
            Self::Read { path, error } => {
                write!(
                    formatter,
                    "could not read authored asset {}: {error}",
                    path.display()
                )
            }
            Self::SizeMismatch {
                path,
                expected,
                actual,
            } => write!(
                formatter,
                "authored asset {} has {actual} bytes, expected {expected}",
                path.display()
            ),
            Self::HashMismatch {
                path,
                expected,
                actual,
            } => write!(
                formatter,
                "authored asset {} has SHA-256 {actual}, expected {expected}",
                path.display()
            ),
            Self::UnmanifestedFile(path) => {
                write!(formatter, "unmanifested authored asset {}", path.display())
            }
            Self::MissingManifestEntry(path) => {
                write!(
                    formatter,
                    "manifest is missing authored asset {}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for ManifestError {}

/// Parse the checked-in manifest, including its schema and duplicate-path
/// invariants.
pub fn entries() -> Result<Vec<ManifestEntry>, ManifestError> {
    let document = serde_json::from_str::<ManifestDocument>(MANIFEST_JSON)
        .map_err(|error| ManifestError::Parse(error.to_string()))?;
    if document.schema != 1 {
        return Err(ManifestError::UnsupportedSchema(document.schema));
    }
    let mut paths = BTreeSet::new();
    for entry in &document.files {
        validate_relative_path(&entry.path)?;
        if !paths.insert(entry.path.clone()) {
            return Err(ManifestError::DuplicatePath(entry.path.clone()));
        }
    }
    Ok(document.files)
}

/// Verify every manifest entry against an extracted ``/app/assets`` root.
///
/// The check is deliberately bidirectional: it catches both changed/missing
/// bytes and new files that were copied into the image without being admitted
/// to the reviewed manifest.
pub fn verify_root(root: impl AsRef<Path>) -> Result<(), ManifestError> {
    let root = root.as_ref();
    let manifest = entries()?;
    let expected = manifest
        .iter()
        .map(|entry| PathBuf::from(&entry.path))
        .collect::<BTreeSet<_>>();
    for entry in &manifest {
        verify_entry(root, entry)?;
    }
    for path in discover_files(root)? {
        if !expected.contains(&path) {
            return Err(ManifestError::UnmanifestedFile(path));
        }
    }
    for path in expected {
        if !root.join(&path).is_file() {
            return Err(ManifestError::MissingManifestEntry(path));
        }
    }
    Ok(())
}

/// Verify one manifest entry.  This smaller boundary makes the hash failure
/// mode directly testable without copying the 185 MiB Dig event-art tree.
pub fn verify_entry(root: impl AsRef<Path>, entry: &ManifestEntry) -> Result<(), ManifestError> {
    validate_relative_path(&entry.path)?;
    let relative = Path::new(&entry.path);
    let path = root.as_ref().join(relative);
    let metadata = fs::metadata(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ManifestError::MissingFile(path.clone())
        } else {
            ManifestError::Read {
                path: path.clone(),
                error: error.to_string(),
            }
        }
    })?;
    if !metadata.is_file() {
        return Err(ManifestError::MissingFile(path));
    }
    let bytes = fs::read(&path).map_err(|error| ManifestError::Read {
        path: path.clone(),
        error: error.to_string(),
    })?;
    if bytes.len() != entry.bytes {
        return Err(ManifestError::SizeMismatch {
            path,
            expected: entry.bytes,
            actual: bytes.len(),
        });
    }
    let actual = sha256_hex(&bytes);
    if actual != entry.sha256 {
        return Err(ManifestError::HashMismatch {
            path,
            expected: entry.sha256.clone(),
            actual,
        });
    }
    Ok(())
}

fn validate_relative_path(path: &str) -> Result<(), ManifestError> {
    let relative = Path::new(path);
    if path.is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
        || !matches!(relative.components().next(), Some(Component::Normal(component)) if component == "dig" || component == "pets")
    {
        return Err(ManifestError::InvalidPath(path.to_owned()));
    }
    Ok(())
}

fn discover_files(root: &Path) -> Result<Vec<PathBuf>, ManifestError> {
    let mut files = Vec::new();
    for top_level in ["dig", "pets"] {
        let directory = root.join(top_level);
        collect_files(root, &directory, &mut files)?;
    }
    files.sort();
    Ok(files)
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), ManifestError> {
    let entries = fs::read_dir(directory).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ManifestError::MissingFile(directory.to_path_buf())
        } else {
            ManifestError::Read {
                path: directory.to_path_buf(),
                error: error.to_string(),
            }
        }
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| ManifestError::Read {
            path: directory.to_path_buf(),
            error: error.to_string(),
        })?;
        let path = entry.path();
        let metadata = entry.metadata().map_err(|error| ManifestError::Read {
            path: path.clone(),
            error: error.to_string(),
        })?;
        if metadata.is_dir() {
            collect_files(root, &path, files)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| ManifestError::InvalidPath(path.to_string_lossy().into_owned()))?;
            files.push(relative.to_path_buf());
        }
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
