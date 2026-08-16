//! Typed read model for the `/dig artifacts` catalog.
//!
//! The existing [`crate::dig_loot::artifact_catalog`] is the Rust-owned
//! identity/order projection used by loot, relic, and prestige policy.  Its
//! compact `ArtifactDef` intentionally does not carry Discord lore/effect
//! prose.  This module joins that projection to a checked generated
//! presentation snapshot, and rejects drift in id/name/layer/rarity/type/
//! prestige before a page can be rendered.  The snapshot is generated from
//! Python by `rust/scripts/generate_dig_artifact_catalog.py`; production Rust
//! never imports or parses Python.
//!
//! The output is a domain [`EmbedModel`] rather than a Discord transport type.
//! A provider can map each page to its transport embed without reimplementing
//! grouping, ordering, or Discord limits.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::OnceLock;

use cama_domain::dig_gear::format_relic_label;
use cama_domain::embed_safety::{
    EmbedField, EmbedModel, FIELD_NAME_LIMIT, FIELD_VALUE_LIMIT, MAX_FIELDS, TITLE_LIMIT,
    TOTAL_LIMIT, truncate_field,
};
use serde::Deserialize;

/// Python's renderer deliberately leaves room for title, description, and
/// footer text while packing fields.  Keep these limits explicit so a future
/// transport cannot accidentally loosen the safety contract.
pub const ARTIFACT_SUBCOMMAND_NAME: &str = "artifacts";
pub const ARTIFACT_SUBCOMMAND_DESCRIPTION: &str = "View all artifacts and the ones you own";
pub const ARTIFACT_FIELDS_PER_PAGE: usize = 20;
pub const ARTIFACT_FIELD_CHARACTER_BUDGET: usize = TOTAL_LIMIT - 700;

const CATALOG_TITLE: &str = "Dig Artifact Catalog";
const CATALOG_DESCRIPTION: &str = "Every discoverable artifact. Relics have equippable effects; curios are collectible keepsakes.";
const OWNED_TITLE: &str = "Your Artifacts";
const OWNED_DESCRIPTION: &str =
    "Artifacts owned by you in this server, repeated from the catalog for quick reference.";
const EMPTY_OWNED_MESSAGE: &str = "You haven't found any artifacts yet.";

/// Authoritative presentation projection for one discoverable artifact.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct ArtifactCatalogDefinition {
    pub id: String,
    pub name: String,
    pub layer: String,
    pub rarity: String,
    pub lore_text: String,
    pub is_relic: bool,
    pub effect: Option<String>,
    pub min_prestige: i64,
}

/// One persisted ownership row.  Duplicate rows are expected: one row is one
/// copy, while `equipped` contributes to the grouped equipped count.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedArtifactRow {
    pub artifact_id: String,
    pub is_relic: bool,
    pub equipped: bool,
}

impl From<&crate::dig_runtime::DigRuntimeArtifact> for OwnedArtifactRow {
    fn from(row: &crate::dig_runtime::DigRuntimeArtifact) -> Self {
        Self {
            artifact_id: row.artifact_id.clone(),
            is_relic: row.is_relic,
            equipped: row.equipped,
        }
    }
}

/// Grouped collection state used to annotate a catalog definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedArtifactSummary {
    pub copies: usize,
    pub equipped: usize,
    pub is_relic: bool,
}

/// Errors indicate a malformed generated snapshot or a drift between that
/// snapshot and the canonical Rust catalog before any Discord-facing page is
/// emitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArtifactCatalogError {
    Snapshot(String),
    RustCatalogDrift(String),
}

impl fmt::Display for ArtifactCatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Snapshot(message) => write!(formatter, "artifact snapshot: {message}"),
            Self::RustCatalogDrift(message) => {
                write!(formatter, "Rust artifact catalog drift: {message}")
            }
        }
    }
}

impl std::error::Error for ArtifactCatalogError {}

/// Group persisted rows by artifact id, preserving the Python semantics that
/// count every row and count equipped rows independently.
#[must_use]
pub fn group_owned_artifacts(rows: &[OwnedArtifactRow]) -> BTreeMap<String, OwnedArtifactSummary> {
    let mut grouped = BTreeMap::new();
    for row in rows {
        if row.artifact_id.is_empty() {
            continue;
        }
        let summary =
            grouped
                .entry(row.artifact_id.clone())
                .or_insert_with(|| OwnedArtifactSummary {
                    copies: 0,
                    equipped: 0,
                    is_relic: false,
                });
        summary.copies += 1;
        summary.equipped += usize::from(row.equipped);
        summary.is_relic |= row.is_relic;
    }
    grouped
}

/// Build pages from the canonical catalog projection and grouped ownership.
/// Catalog pages are always emitted first; owned pages are always emitted,
/// including one empty page for a player with no rows.
#[must_use]
pub fn build_artifact_catalog_pages(
    catalog: &[ArtifactCatalogDefinition],
    owned_rows: &[OwnedArtifactRow],
) -> Vec<EmbedModel> {
    let grouped = group_owned_artifacts(owned_rows);

    let catalog_fields = catalog
        .iter()
        .map(|artifact| definition_field(artifact, None))
        .collect::<Vec<_>>();
    let catalog_pages = paginate_fields(
        catalog_fields,
        CATALOG_TITLE,
        CATALOG_DESCRIPTION,
        format!("Catalog: {} artifacts", catalog.len()),
        None,
    );

    let mut known_owned = BTreeMap::new();
    let mut owned_fields = Vec::new();
    for artifact in catalog {
        if let Some(summary) = grouped.get(&artifact.id) {
            known_owned.insert(artifact.id.as_str(), ());
            owned_fields.push(definition_field(artifact, Some(summary)));
        }
    }

    // Preserve the Python fallback behavior for generated/retired rows that
    // no longer have a current catalog definition.  BTreeMap gives the same
    // deterministic lexical order as Python's sorted(owned.items()).
    for (artifact_id, summary) in &grouped {
        if known_owned.contains_key(artifact_id.as_str()) {
            continue;
        }
        owned_fields.push(fallback_field(artifact_id, summary));
    }

    let owned_pages = paginate_fields(
        owned_fields,
        OWNED_TITLE,
        OWNED_DESCRIPTION,
        format!("Your collection: {} unique artifacts", grouped.len()),
        Some(EMPTY_OWNED_MESSAGE),
    );

    catalog_pages.into_iter().chain(owned_pages).collect()
}

/// Build the complete read model from the actual Rust artifact catalog and
/// authoritative Python prose.  The returned vector is owned so callers can
/// safely cross a blocking repository/Discord boundary.
pub fn build_canonical_artifact_catalog_pages(
    owned_rows: &[OwnedArtifactRow],
) -> Result<Vec<EmbedModel>, ArtifactCatalogError> {
    let catalog = canonical_artifact_catalog()?;
    Ok(build_artifact_catalog_pages(&catalog, owned_rows))
}

/// Project the current runtime snapshot rows into this read model's narrow
/// ownership type.  The provider can call this after one scoped snapshot
/// query, avoiding a second ownership query or a transport-shaped row map.
#[must_use]
pub fn owned_rows_from_runtime(
    rows: &[crate::dig_runtime::DigRuntimeArtifact],
) -> Vec<OwnedArtifactRow> {
    rows.iter().map(OwnedArtifactRow::from).collect()
}

fn definition_field(
    artifact: &ArtifactCatalogDefinition,
    owned: Option<&OwnedArtifactSummary>,
) -> EmbedField {
    let ownership = owned.map_or_else(String::new, |summary| {
        let equipped = if summary.equipped > 0 {
            format!(" • Equipped ×{}", summary.equipped)
        } else {
            String::new()
        };
        format!(" — Owned ×{}{}", summary.copies, equipped)
    });
    let name = truncate_field(&format!("{}{}", artifact.name, ownership), FIELD_NAME_LIMIT);
    let artifact_type = if artifact.is_relic { "Relic" } else { "Curio" };
    let mut metadata = format!(
        "**{}** • **{}** • **{}**",
        artifact.rarity, artifact.layer, artifact_type
    );
    if artifact.min_prestige > 0 {
        metadata.push_str(&format!(" • **Prestige P{}+**", artifact.min_prestige));
    }
    let effect = artifact
        .effect
        .as_deref()
        .unwrap_or("Collectible only (no equip effect.)");
    let value = truncate_field(
        &format!("{metadata}\n*{}*\n**Effect:** {effect}", artifact.lore_text),
        FIELD_VALUE_LIMIT,
    );
    EmbedField {
        name,
        value,
        inline: false,
    }
}

fn fallback_field(artifact_id: &str, summary: &OwnedArtifactSummary) -> EmbedField {
    let label = format_relic_label(artifact_id, true);
    let ownership = if summary.equipped > 0 {
        format!(
            " — Owned ×{} • Equipped ×{}",
            summary.copies, summary.equipped
        )
    } else {
        format!(" — Owned ×{}", summary.copies)
    };
    let name = truncate_field(&format!("{label}{ownership}"), FIELD_NAME_LIMIT);
    let (rarity, layer, lore, effect) = if artifact_id.starts_with("pinnacle:") {
        (
            "Unique",
            "Special reward",
            "A one-of-a-kind artifact whose properties were rolled when it was found.",
            "Its generated effects are listed in the artifact name.",
        )
    } else {
        (
            "Legacy",
            "Unknown layer",
            "This owned artifact is no longer present in the current catalog.",
            "No current effect description is available.",
        )
    };
    let artifact_type = if summary.is_relic { "Relic" } else { "Curio" };
    let value = truncate_field(
        &format!(
            "**{rarity}** • **{layer}** • **{artifact_type}**\n*{lore}*\n**Effect:** {effect}"
        ),
        FIELD_VALUE_LIMIT,
    );
    EmbedField {
        name,
        value,
        inline: false,
    }
}

fn paginate_fields(
    fields: Vec<EmbedField>,
    title: &str,
    description: &str,
    footer_label: String,
    empty_message: Option<&str>,
) -> Vec<EmbedModel> {
    let mut pages = Vec::<Vec<EmbedField>>::new();
    let mut current = Vec::new();
    let mut current_characters = 0;
    for field in fields {
        let field_characters = field.name.chars().count() + field.value.chars().count();
        if !current.is_empty()
            && (current.len() >= ARTIFACT_FIELDS_PER_PAGE
                || current_characters + field_characters > ARTIFACT_FIELD_CHARACTER_BUDGET)
        {
            pages.push(current);
            current = Vec::new();
            current_characters = 0;
        }
        current_characters += field_characters;
        current.push(field);
    }
    if !current.is_empty() || pages.is_empty() {
        pages.push(current);
    }

    let page_count = pages.len();
    pages
        .into_iter()
        .enumerate()
        .map(|(index, fields)| {
            let page_number = index + 1;
            let page_title = if page_count > 1 {
                format!("{title} ({page_number}/{page_count})")
            } else {
                title.to_owned()
            };
            let page_description = if page_number == 1 {
                Some(description.to_owned())
            } else {
                None
            };
            let mut embed = EmbedModel {
                title: Some(truncate_field(&page_title, TITLE_LIMIT)),
                description: page_description,
                footer: Some(format!("{footer_label} • Page {page_number}/{page_count}")),
                ..EmbedModel::default()
            };
            if fields.is_empty() {
                if let Some(empty_message) = empty_message {
                    embed.description = Some(format!("{description}\n\n{empty_message}"));
                }
            } else {
                embed.fields = fields;
            }
            debug_assert!(embed.fields.len() <= MAX_FIELDS);
            debug_assert!(
                embed
                    .title
                    .as_deref()
                    .is_none_or(|value| value.len() <= TITLE_LIMIT)
            );
            debug_assert!(cama_domain::embed_safety::validate_embed(&embed).is_empty());
            embed
        })
        .collect()
}

/// Stable FNV-1a checksum for the generated presentation snapshot.
#[must_use]
pub fn canonical_artifact_snapshot_hash() -> u64 {
    CANONICAL_ARTIFACT_JSON
        .bytes()
        .fold(0xcbf29ce484222325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        })
}

/// Parse the generated presentation snapshot once per process. The Rust
/// catalog remains the source of identity/order truth; this snapshot only
/// carries the prose fields that its compact policy projection omits.
fn canonical_artifact_snapshot()
-> Result<&'static [ArtifactCatalogDefinition], ArtifactCatalogError> {
    static CATALOG: OnceLock<Result<Vec<ArtifactCatalogDefinition>, ArtifactCatalogError>> =
        OnceLock::new();
    CATALOG
        .get_or_init(|| {
            serde_json::from_str(CANONICAL_ARTIFACT_JSON)
                .map_err(|error| ArtifactCatalogError::Snapshot(error.to_string()))
        })
        .as_deref()
        .map_err(Clone::clone)
}

/// Return the generated presentation catalog after validating its ID/order
/// and gameplay metadata against the canonical Rust catalog.
pub fn canonical_artifact_catalog() -> Result<Vec<ArtifactCatalogDefinition>, ArtifactCatalogError>
{
    let snapshot = canonical_artifact_snapshot()?;
    let rust_rows = crate::dig_loot::artifact_catalog();
    if snapshot.len() != rust_rows.len() {
        return Err(ArtifactCatalogError::RustCatalogDrift(format!(
            "row count differs (snapshot {}, Rust {})",
            snapshot.len(),
            rust_rows.len()
        )));
    }
    for (index, (presentation, rust)) in snapshot.iter().zip(rust_rows).enumerate() {
        let rust_rarity = match rust.rarity {
            crate::dig_loot::Rarity::Common => "Common",
            crate::dig_loot::Rarity::Uncommon => "Uncommon",
            crate::dig_loot::Rarity::Rare => "Rare",
            crate::dig_loot::Rarity::Legendary => "Legendary",
        };
        if presentation.id != rust.id
            || presentation.name != rust.name
            || presentation.layer != rust.layer
            || presentation.rarity != rust_rarity
            || presentation.is_relic != rust.is_relic
            || presentation.min_prestige != rust.min_prestige
        {
            return Err(ArtifactCatalogError::RustCatalogDrift(format!(
                "row {index} differs for {}",
                presentation.id
            )));
        }
    }
    Ok(snapshot.to_vec())
}

const CANONICAL_ARTIFACT_JSON: &str = include_str!("dig_artifact_catalog.json");

#[cfg(test)]
mod tests {
    use super::*;
    use cama_domain::embed_safety::validate_embed;

    fn catalog() -> Vec<ArtifactCatalogDefinition> {
        canonical_artifact_catalog().expect("canonical artifact catalog")
    }

    fn owned(id: &str, is_relic: bool, equipped: bool) -> OwnedArtifactRow {
        OwnedArtifactRow {
            artifact_id: id.to_owned(),
            is_relic,
            equipped,
        }
    }

    fn text(embed: &EmbedModel) -> String {
        let mut text = String::new();
        if let Some(title) = &embed.title {
            text.push_str(title);
            text.push('\n');
        }
        if let Some(description) = &embed.description {
            text.push_str(description);
            text.push('\n');
        }
        for field in &embed.fields {
            text.push_str(&field.name);
            text.push('\n');
            text.push_str(&field.value);
            text.push('\n');
        }
        if let Some(footer) = &embed.footer {
            text.push_str(footer);
        }
        text
    }

    fn assert_safe(pages: &[EmbedModel]) {
        assert!(!pages.is_empty());
        for page in pages {
            assert!(validate_embed(page).is_empty(), "unsafe page: {page:?}");
            assert!(page.fields.len() <= ARTIFACT_FIELDS_PER_PAGE);
            assert!(page.fields.len() <= MAX_FIELDS);
        }
    }

    #[test]
    fn test_artifacts_subcommand_contract_is_stable() {
        assert_eq!(ARTIFACT_SUBCOMMAND_NAME, "artifacts");
        assert!(ARTIFACT_SUBCOMMAND_DESCRIPTION.contains("artifact"));
    }

    #[test]
    fn test_artifact_snapshot_hash_and_generator_gate_are_current() {
        assert_eq!(canonical_artifact_snapshot_hash(), 0x0f6f_f06a_ec87_ac21);
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let script = [
            manifest.join("../scripts/generate_dig_artifact_catalog.py"),
            manifest.join("../../scripts/generate_dig_artifact_catalog.py"),
        ]
        .into_iter()
        .find(|path| path.is_file())
        .expect("artifact snapshot generator");
        let output = std::process::Command::new("python3")
            .arg(script)
            .arg("--check")
            .output()
            .expect("python3 is required for the artifact snapshot drift gate");
        assert!(
            output.status.success(),
            "canonical Dig artifact snapshot is stale: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn test_artifacts_describes_catalog_and_repeats_owned_at_bottom() {
        let catalog = catalog();
        let pages = build_artifact_catalog_pages(
            &catalog,
            &[
                owned("map_of_the_first_descent", false, false),
                owned("mole_claws", true, false),
            ],
        );
        let rendered = pages.iter().map(text).collect::<Vec<_>>().join("\n");
        for artifact in &catalog {
            assert!(rendered.contains(&artifact.name));
            assert!(rendered.contains(&artifact.layer));
            assert!(rendered.contains(&artifact.rarity));
            assert!(rendered.contains(&artifact.lore_text));
            if let Some(effect) = &artifact.effect {
                assert!(rendered.contains(effect));
            }
            let expected_count =
                if artifact.id == "mole_claws" || artifact.id == "map_of_the_first_descent" {
                    2
                } else {
                    1
                };
            assert_eq!(rendered.matches(&artifact.name).count(), expected_count);
        }
        assert!(pages.last().is_some_and(|page| {
            page.title
                .as_deref()
                .is_some_and(|title| title.contains("Your Artifacts"))
        }));
        assert_safe(&pages);
    }

    #[test]
    fn test_artifacts_scopes_owned_rows_and_paginates_safely() {
        let pages = build_canonical_artifact_catalog_pages(&[owned("echo_stone", true, true)])
            .expect("catalog pages");
        assert!(pages.len() > 1);
        let owned_text = pages
            .iter()
            .filter(|page| {
                page.title
                    .as_deref()
                    .is_some_and(|title| title.contains("Your Artifacts"))
            })
            .map(text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(owned_text.contains("Equipped ×1"));
        assert_safe(&pages);
    }

    #[test]
    fn test_artifacts_paginates_full_owned_collection_after_catalog() {
        let catalog = catalog();
        let rows = catalog
            .iter()
            .map(|artifact| owned(&artifact.id, artifact.is_relic, false))
            .collect::<Vec<_>>();
        let pages = build_artifact_catalog_pages(&catalog, &rows);
        let first_owned = pages
            .iter()
            .position(|page| {
                page.title
                    .as_deref()
                    .is_some_and(|title| title.contains("Your Artifacts"))
            })
            .expect("owned pages");
        assert!(first_owned > 0);
        assert!(pages[first_owned..].len() > 1);
        let catalog_text = pages[..first_owned]
            .iter()
            .map(text)
            .collect::<Vec<_>>()
            .join("\n");
        let owned_text = pages[first_owned..]
            .iter()
            .map(text)
            .collect::<Vec<_>>()
            .join("\n");
        for artifact in &catalog {
            assert!(catalog_text.contains(&artifact.name));
            assert!(catalog_text.contains(&artifact.lore_text));
            assert!(owned_text.contains(&artifact.name));
            assert!(owned_text.contains(&artifact.lore_text));
            if let Some(effect) = &artifact.effect {
                assert!(owned_text.contains(effect));
            }
        }
        assert_safe(&pages);
    }

    #[test]
    fn test_artifacts_groups_duplicate_owned_rows() {
        let pages = build_canonical_artifact_catalog_pages(&[
            owned("mole_claws", true, false),
            owned("mole_claws", true, false),
        ])
        .expect("catalog pages");
        let first_owned = pages
            .iter()
            .position(|page| {
                page.title
                    .as_deref()
                    .is_some_and(|title| title.contains("Your Artifacts"))
            })
            .expect("owned pages");
        let owned_text = pages[first_owned..]
            .iter()
            .map(text)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(owned_text.matches("Mole Claws").count(), 1);
        assert!(owned_text.contains("Owned ×2"));
        assert_safe(&pages);
    }

    #[test]
    fn test_artifacts_has_bottom_section_for_empty_collection() {
        let pages = build_canonical_artifact_catalog_pages(&[]).expect("catalog pages");
        let last = pages.last().expect("owned page");
        assert!(
            last.title
                .as_deref()
                .is_some_and(|title| title.contains("Your Artifacts"))
        );
        assert!(text(last).to_ascii_lowercase().contains("haven't found"));
        assert_safe(&pages);
    }
}
