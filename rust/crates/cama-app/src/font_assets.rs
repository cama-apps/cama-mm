//! Fixed DejaVu font assets retained by the Rust runtime image.
//!
//! Python's shared loader resolves DejaVu from the host font directory.  A
//! host lookup is deliberately not a suitable contract for Rust image work:
//! package upgrades or a different base image can silently change glyph
//! metrics.  `Dockerfile.rust` copies the four exact files from the locked
//! Matplotlib dependency and verifies [`FONT_MANIFEST`] while building the
//! runtime image. This module supplies the production renderers and repeats the
//! check at load time so an incomplete or mutated deployment fails closed.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

/// Runtime location populated by `Dockerfile.rust`.
pub const RUNTIME_FONT_DIR: &str = "/app/assets/fonts";
pub const FONT_DIR_ENV: &str = "CAMA_FONT_DIR";

#[cfg(test)]
const FONT_MANIFEST: &str = include_str!("../../../../assets/fonts/dejavu.sha256");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DejaVuFace {
    Sans,
    SansBold,
    SansMono,
    SansMonoBold,
}

impl DejaVuFace {
    pub const ALL: [Self; 4] = [
        Self::Sans,
        Self::SansBold,
        Self::SansMono,
        Self::SansMonoBold,
    ];

    pub const fn file_name(self) -> &'static str {
        match self {
            Self::Sans => "DejaVuSans.ttf",
            Self::SansBold => "DejaVuSans-Bold.ttf",
            Self::SansMono => "DejaVuSansMono.ttf",
            Self::SansMonoBold => "DejaVuSansMono-Bold.ttf",
        }
    }

    const fn expected_sha256(self) -> &'static str {
        match self {
            Self::Sans => "3fdf69cabf06049ea70a00b5919340e2ce1e6d02b0cc3c4b44fb6801bd1e0d22",
            Self::SansBold => "b184b89e3c1075f22f6b71575b6fc20d4972b3cfd3b23322ca6fd596dcaef167",
            Self::SansMono => "602ec86b8948cfcd956482fe64f94c36c867770149ef2f791d4613f443bcecb3",
            Self::SansMonoBold => {
                "baada9a5172fe20886251aff0433fc38461912d5daf07287e7bee56620a8da96"
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum FontAssetError {
    #[error("unable to read fixed DejaVu font {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("fixed DejaVu font {face} has SHA-256 {actual}, expected {expected}")]
    HashMismatch {
        face: &'static str,
        actual: String,
        expected: &'static str,
    },
}

/// Load a fixed DejaVu face from the Rust runtime's retained asset directory.
///
/// The returned bytes are intentionally unparsed so each renderer can choose
/// the appropriate face and size without taking a host font dependency.
pub fn load_dejavu_font(face: DejaVuFace) -> Result<Vec<u8>, FontAssetError> {
    let directory = std::env::var_os(FONT_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(RUNTIME_FONT_DIR));
    load_dejavu_font_from(&directory, face)
}

/// Testable/configurable form of [`load_dejavu_font`].
pub fn load_dejavu_font_from(
    directory: &Path,
    face: DejaVuFace,
) -> Result<Vec<u8>, FontAssetError> {
    let path = directory.join(face.file_name());
    let bytes = std::fs::read(&path).map_err(|source| FontAssetError::Io {
        path: path.clone(),
        source,
    })?;
    let actual = sha256_hex(&bytes);
    if actual != face.expected_sha256() {
        return Err(FontAssetError::HashMismatch {
            face: face.file_name(),
            actual,
            expected: face.expected_sha256(),
        });
    }
    Ok(bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn manifest_matches_every_loader_face() {
        let entries = FONT_MANIFEST
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let mut fields = line.split_whitespace();
                let hash = fields.next().expect("manifest hash");
                let file_name = fields.next().expect("manifest file name");
                assert!(fields.next().is_none(), "manifest line has extra fields");
                (file_name.to_owned(), hash.to_owned())
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(entries.len(), DejaVuFace::ALL.len());
        for face in DejaVuFace::ALL {
            assert_eq!(
                entries.get(face.file_name()).map(String::as_str),
                Some(face.expected_sha256())
            );
        }
    }

    #[test]
    fn rust_image_retains_only_the_checked_font_assets() {
        let dockerfile = include_str!("../../../../Dockerfile.rust");
        let python_dockerfile = include_str!("../../../../Dockerfile");
        assert!(dockerfile.contains("COPY assets/fonts/dejavu.sha256 /build/dejavu.sha256"));
        assert!(dockerfile.contains("sha256sum -c /build/dejavu.sha256"));
        assert!(dockerfile.contains("/build/font-assets/ /app/assets/fonts/"));
        assert!(dockerfile.contains("LICENSE_DEJAVU"));
        assert!(
            python_dockerfile.contains("COPY assets/fonts/dejavu.sha256 /tmp/cama-dejavu.sha256")
        );
        assert!(python_dockerfile.contains("sha256sum -c /tmp/cama-dejavu.sha256"));
        assert!(python_dockerfile.contains("/usr/share/fonts/truetype/dejavu/$font"));
        assert!(
            !dockerfile.contains("fonts-dejavu-core"),
            "Rust image must not fall back to an unpinned host font package"
        );
        for face in DejaVuFace::ALL {
            assert!(
                dockerfile.contains(face.file_name()),
                "Dockerfile retains {}",
                face.file_name()
            );
        }
    }

    #[test]
    fn loader_rejects_missing_font_instead_of_falling_back() {
        let directory = tempdir().expect("temporary font directory");
        let error = load_dejavu_font_from(directory.path(), DejaVuFace::Sans)
            .expect_err("missing fixed font must fail closed");
        assert!(matches!(error, FontAssetError::Io { .. }));
    }

    #[test]
    fn loader_rejects_mutated_font_bytes() {
        let directory = tempdir().expect("temporary font directory");
        std::fs::write(
            directory.path().join(DejaVuFace::Sans.file_name()),
            b"not-a-font",
        )
        .expect("write mutation");
        let error = load_dejavu_font_from(directory.path(), DejaVuFace::Sans)
            .expect_err("mutated fixed font must fail closed");
        assert!(matches!(error, FontAssetError::HashMismatch { .. }));
    }
}
