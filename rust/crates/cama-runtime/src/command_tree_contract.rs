//! Contract checks for the composed Discord application-command tree.
//!
//! The Python bot's command tree is part of its public API.  This module keeps
//! the corresponding Rust check at the `Registry` boundary, after providers
//! have emitted their typed [`CommandSpec`] values.  It deliberately does not
//! inspect source files or provider names: callers pass the registry that will
//! be handed to the Discord transport.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::registration::{
    CommandOptionKind, CommandOptionSpec, DISCORD_COMMAND_OPTION_LIMIT,
    DISCORD_GLOBAL_COMMAND_LIMIT, Registry,
};

const REPRESENTATIVE_ROOTS: &[&str] = &[
    "player",
    "draft",
    "predict",
    "duel",
    "admin",
    "bet",
    "balance",
    "gamba",
    "economy",
    "enrich",
    "matches",
    "dota",
    "shop",
    "lobby",
    "shuffle",
    "record",
    "profile",
    "leaderboard",
    "help",
    "blameluke",
    "refer",
    "survey",
];

const APPROVED_PATHS: &[&str] = &[
    "/dig admin resetcooldown",
    "/dig admin forceevent",
    "/dig admin setdepth",
    "/pet adopt",
    "/pet status",
    "/pet feed",
    "/pet shop",
    "/pet buy",
    "/pet rename",
    "/pet graveyard",
    "/pet leaderboard",
    "/pet trinket",
    "/pet brawl",
    "/pet altar",
    "/pet eat",
    "/economy tip",
    "/economy paydebt",
    "/economy bankruptcy",
    "/economy loan",
    "/economy reserve",
    "/economy disburse",
    "/economy invest",
    "/shop buy",
    "/shop pingedash",
    "/shop pingedkevin",
    "/shop avoids",
    "/shop deals",
    "/shop mana",
    "/matches history",
    "/matches view",
    "/matches recent",
    "/player lobby autonotify",
    "/player lobby status",
    "/admin lowprio add",
    "/admin lowprio remove",
    "/admin lowprio status",
    "/admin lowprio list",
    "/admin moderation suspend",
    "/admin moderation lift",
    "/admin moderation status",
    "/admin moderation list",
    "/admin moderation history",
    "/blameluke",
    "/survey create",
    "/survey list",
    "/survey preview",
    "/survey delete",
    "/survey send",
    "/survey retry",
    "/survey results",
    "/survey close",
    "/survey question add",
    "/survey question edit",
    "/survey question remove",
    "/survey question move",
];

const RETIRED_PATHS: &[&str] = &[
    "/dig resetcooldown",
    "/dig forceevent",
    "/dig setdepth",
    "/tip",
    "/paydebt",
    "/bankruptcy",
    "/loan",
    "/nonprofit",
    "/disburse",
    "/shop",
    "/pingedash",
    "/pingedkevin",
    "/myavoids",
    "/mydeals",
    "/manashop",
    "/matchhistory",
    "/viewmatch",
    "/recent",
];

/// A read-only summary of the command tree emitted by a [`Registry`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandTreeSnapshot {
    /// Names of all top-level command specs, including group roots.
    pub top_level_commands: BTreeSet<String>,
    /// Invokable command paths.  Group-only nodes are intentionally omitted,
    /// matching Discord's `walk_commands` behavior.
    pub command_paths: BTreeSet<String>,
    /// Number of direct options on every root or nested option that owns
    /// options.  Keys use slash-qualified paths (for example `/dig admin`).
    pub direct_option_counts: BTreeMap<String, usize>,
}

/// A failed production command-tree contract check.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CommandTreeContractError {
    #[error("missing representative top-level commands: {0:?}")]
    MissingRepresentativeRoots(Vec<String>),
    #[error("missing approved command paths: {0:?}")]
    MissingApprovedPaths(Vec<String>),
    #[error("retired command paths are still registered: {0:?}")]
    RetiredPathsPresent(Vec<String>),
    #[error(
        "expected {expected} top-level commands, found {actual} (ask present: {ask_present}, pet expected: {pet_expected})"
    )]
    UnexpectedTopLevelCount {
        expected: usize,
        actual: usize,
        ask_present: bool,
        pet_expected: bool,
    },
    #[error("command path {path:?} has {actual} direct options; Discord permits at most {limit}")]
    OptionLimitExceeded {
        path: String,
        actual: usize,
        limit: usize,
    },
    #[error("command path {path:?} has {actual} direct options; expected {expected}")]
    DirectOptionCountMismatch {
        path: String,
        expected: usize,
        actual: usize,
    },
}

/// Build a recursive, transport-independent snapshot of a registry.
#[must_use]
pub fn snapshot_registry(registry: &Registry) -> CommandTreeSnapshot {
    let mut snapshot = CommandTreeSnapshot {
        top_level_commands: BTreeSet::new(),
        command_paths: BTreeSet::new(),
        direct_option_counts: BTreeMap::new(),
    };

    for command in registry.commands() {
        snapshot.top_level_commands.insert(command.name.clone());
        let root_path = format!("/{}", command.name);
        if command.options.is_empty() || command.options.iter().all(|option| !is_group(option.kind))
        {
            snapshot.command_paths.insert(root_path.clone());
        }
        if !command.options.is_empty() {
            snapshot
                .direct_option_counts
                .insert(root_path.clone(), command.options.len());
        }
        collect_nested_snapshot(&mut snapshot, &root_path, &command.options);
    }

    snapshot
}

/// Validate the command tree that is about to be submitted to Discord.
///
/// This is the production-callable seam for the final command-tree checks.
/// The caller should pass the fully composed registry, not an individual
/// provider or a source-derived list.  `ask` remains conditional in Python,
/// and `pet` is independently conditional on `PET_CHANNEL_ID`.  The caller
/// supplies whether the Pet provider is expected from that startup
/// configuration so a missing provider still fails when the channel is set.
pub fn validate_production_registry(
    registry: &Registry,
    pet_expected: bool,
) -> Result<CommandTreeSnapshot, CommandTreeContractError> {
    let snapshot = snapshot_registry(registry);

    let missing_roots = REPRESENTATIVE_ROOTS
        .iter()
        .filter(|root| !snapshot.top_level_commands.contains(**root))
        .map(|root| (*root).to_owned())
        .collect::<Vec<_>>();
    if !missing_roots.is_empty() {
        return Err(CommandTreeContractError::MissingRepresentativeRoots(
            missing_roots,
        ));
    }

    let missing_paths = APPROVED_PATHS
        .iter()
        .filter(|path| pet_expected || !path.starts_with("/pet "))
        .filter(|path| !snapshot.command_paths.contains(**path))
        .map(|path| (*path).to_owned())
        .collect::<Vec<_>>();
    if !missing_paths.is_empty() {
        return Err(CommandTreeContractError::MissingApprovedPaths(
            missing_paths,
        ));
    }

    let retired_paths = RETIRED_PATHS
        .iter()
        .filter(|path| snapshot.command_paths.contains(**path))
        .map(|path| (*path).to_owned())
        .collect::<Vec<_>>();
    if !retired_paths.is_empty() {
        return Err(CommandTreeContractError::RetiredPathsPresent(retired_paths));
    }

    let ask_present = snapshot.top_level_commands.contains("ask");
    let expected_top_level_count = 43 + usize::from(ask_present) + usize::from(pet_expected);
    if snapshot.top_level_commands.len() != expected_top_level_count {
        return Err(CommandTreeContractError::UnexpectedTopLevelCount {
            expected: expected_top_level_count,
            actual: snapshot.top_level_commands.len(),
            ask_present,
            pet_expected,
        });
    }
    if snapshot.top_level_commands.len() > DISCORD_GLOBAL_COMMAND_LIMIT {
        return Err(CommandTreeContractError::OptionLimitExceeded {
            path: "/".to_owned(),
            actual: snapshot.top_level_commands.len(),
            limit: DISCORD_GLOBAL_COMMAND_LIMIT,
        });
    }

    if let Some((path, actual)) = snapshot
        .direct_option_counts
        .iter()
        .find(|(_, count)| **count > DISCORD_COMMAND_OPTION_LIMIT)
    {
        return Err(CommandTreeContractError::OptionLimitExceeded {
            path: path.clone(),
            actual: *actual,
            limit: DISCORD_COMMAND_OPTION_LIMIT,
        });
    }

    for (path, expected) in [
        ("/dig", 22),
        ("/player", 8),
        ("/player lobby", 2),
        ("/survey", 9),
        ("/survey question", 4),
    ] {
        let actual = snapshot
            .direct_option_counts
            .get(path)
            .copied()
            .unwrap_or_default();
        if actual != expected {
            return Err(CommandTreeContractError::DirectOptionCountMismatch {
                path: path.to_owned(),
                expected,
                actual,
            });
        }
    }

    Ok(snapshot)
}

fn is_group(kind: CommandOptionKind) -> bool {
    matches!(
        kind,
        CommandOptionKind::Subcommand | CommandOptionKind::SubcommandGroup
    )
}

fn collect_nested_snapshot(
    snapshot: &mut CommandTreeSnapshot,
    parent_path: &str,
    options: &[CommandOptionSpec],
) {
    for option in options {
        let path = format!("{parent_path} {}", option.name);
        if option.kind == CommandOptionKind::Subcommand {
            snapshot.command_paths.insert(path.clone());
        }
        if !option.options.is_empty() {
            snapshot
                .direct_option_counts
                .insert(path.clone(), option.options.len());
        }
        collect_nested_snapshot(snapshot, &path, &option.options);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::*;
    use crate::registration::{
        CommandSpec, InteractionHandler, InteractionHandlerError, InteractionRequest,
        InteractionResponder, RegistryBuilder,
    };

    struct FixtureHandler;

    #[async_trait]
    impl InteractionHandler for FixtureHandler {
        async fn handle(
            &self,
            _request: InteractionRequest,
            _responder: Arc<dyn InteractionResponder>,
        ) -> Result<(), InteractionHandlerError> {
            Ok(())
        }
    }

    fn option(name: &str) -> CommandOptionSpec {
        CommandOptionSpec::new(name, "Fixture command", CommandOptionKind::Subcommand)
    }

    fn group(name: &str, children: Vec<CommandOptionSpec>) -> CommandOptionSpec {
        CommandOptionSpec::new(
            name,
            "Fixture command group",
            CommandOptionKind::SubcommandGroup,
        )
        .options(children)
    }

    fn command(name: &str, options: Vec<CommandOptionSpec>) -> CommandSpec {
        CommandSpec {
            name: name.to_owned(),
            description: "Fixture production command".to_owned(),
            options,
            handler: Arc::new(FixtureHandler),
        }
    }

    fn options(names: &[&str]) -> Vec<CommandOptionSpec> {
        names.iter().map(|name| option(name)).collect()
    }

    /// This contract fixture mirrors the final composed provider shape at the
    /// typed Registry boundary.  Provider constructors require live database,
    /// OpenDota, Discord, and AI ports, so the exact integration of those
    /// constructors belongs in the production composition test.  These tests
    /// intentionally exercise the validator against a complete command tree,
    /// including all approved paths and nested option limits, rather than
    /// claiming that this fixture itself boots the external service graph.
    fn fixture_registry(with_ask: bool, with_pet: bool) -> Registry {
        let mut builder = RegistryBuilder::default();

        builder
            .command(command(
                "dig",
                vec![
                    option("go"),
                    option("help"),
                    option("sabotage"),
                    option("info"),
                    option("leaderboard"),
                    option("halloffame"),
                    option("use"),
                    option("gift"),
                    option("shop"),
                    option("buy"),
                    option("flex"),
                    option("prestige"),
                    option("abandon"),
                    option("trap"),
                    option("insure"),
                    option("inventory"),
                    option("artifacts"),
                    option("gear"),
                    option("weather"),
                    option("guide"),
                    group(
                        "admin",
                        options(&["resetcooldown", "forceevent", "setdepth"]),
                    ),
                    group(
                        "miner",
                        options(&["profile", "about", "build", "respec", "autobuy"]),
                    ),
                ],
            ))
            .expect("dig fixture");
        builder
            .command(command(
                "player",
                vec![
                    option("exclusion"),
                    option("link"),
                    group("lobby", options(&["autonotify", "status"])),
                    option("region"),
                    option("register"),
                    option("roles"),
                    option("steamids"),
                    option("unlink"),
                ],
            ))
            .expect("player fixture");
        builder
            .command(command(
                "survey",
                vec![
                    option("create"),
                    option("list"),
                    option("preview"),
                    option("delete"),
                    option("send"),
                    option("retry"),
                    option("results"),
                    option("close"),
                    group("question", options(&["add", "edit", "remove", "move"])),
                ],
            ))
            .expect("survey fixture");
        builder
            .command(command(
                "economy",
                options(&[
                    "tip",
                    "paydebt",
                    "bankruptcy",
                    "loan",
                    "reserve",
                    "disburse",
                    "invest",
                ]),
            ))
            .expect("economy fixture");
        builder
            .command(command(
                "shop",
                options(&["buy", "pingedash", "pingedkevin", "avoids", "deals", "mana"]),
            ))
            .expect("shop fixture");
        builder
            .command(command("matches", options(&["history", "view", "recent"])))
            .expect("matches fixture");
        builder
            .command(command(
                "admin",
                vec![
                    option("addfake"),
                    option("addsteamid"),
                    option("bumprd"),
                    option("correctmatch"),
                    option("extendbetting"),
                    option("filllobbytest"),
                    option("givecoin"),
                    option("health"),
                    option("recalibrate"),
                    option("registeruser"),
                    option("removesteamid"),
                    option("resetbankruptcycooldown"),
                    option("resetloancooldown"),
                    option("resetrecalibrationcooldown"),
                    option("resetuser"),
                    option("seedherogrid"),
                    option("setprimarysteam"),
                    option("setrating"),
                    option("sync"),
                    group("lowprio", options(&["add", "remove", "status", "list"])),
                    group(
                        "moderation",
                        options(&["suspend", "lift", "status", "list", "history"]),
                    ),
                ],
            ))
            .expect("admin fixture");
        if with_pet {
            builder
                .command(command(
                    "pet",
                    options(&[
                        "adopt",
                        "status",
                        "feed",
                        "shop",
                        "buy",
                        "rename",
                        "graveyard",
                        "leaderboard",
                        "trinket",
                        "brawl",
                        "altar",
                        "eat",
                        "train",
                    ]),
                ))
                .expect("pet fixture");
        }

        for root in REPRESENTATIVE_ROOTS.iter().copied().filter(|root| {
            !matches!(
                *root,
                "player" | "survey" | "economy" | "shop" | "matches" | "admin"
            )
        }) {
            if !["dig", "pet"].contains(&root) {
                builder
                    .command(command(root, Vec::new()))
                    .expect("representative root fixture");
            }
        }

        // The remaining roots are the other production providers.  Keeping
        // their actual names here makes the top-level count check meaningful
        // without pretending their external ports are initialized in a unit
        // test.
        for root in [
            "wrapped",
            "tax",
            "scout",
            "mafia",
            "mana",
            "trivia",
            "herogrid",
            "matchup",
            "playertrivia",
            "trivia-reset-cooldown",
            "calibration",
            "kick",
            "join",
            "leave",
            "readycheck",
            "resetlobby",
            "setreminder",
            "mybets",
            "bets",
            "ratinganalysis",
        ] {
            builder
                .command(command(root, Vec::new()))
                .expect("additional root fixture");
        }
        if with_ask {
            builder
                .command(command("ask", Vec::new()))
                .expect("ask fixture");
        }

        builder.build()
    }

    #[test]
    fn production_registry_uses_approved_consolidated_paths() {
        let registry = fixture_registry(false, true);
        let snapshot =
            validate_production_registry(&registry, true).expect("valid command contract");
        assert!(
            APPROVED_PATHS
                .iter()
                .all(|path| snapshot.command_paths.contains(*path))
        );
        assert!(
            RETIRED_PATHS
                .iter()
                .all(|path| !snapshot.command_paths.contains(*path))
        );
    }

    #[test]
    fn production_registry_stays_within_discord_limits() {
        let registry = fixture_registry(true, true);
        let snapshot =
            validate_production_registry(&registry, true).expect("valid command contract");
        assert_eq!(snapshot.top_level_commands.len(), 45);
        assert!(
            snapshot
                .direct_option_counts
                .values()
                .all(|count| *count <= DISCORD_COMMAND_OPTION_LIMIT)
        );
        assert_eq!(snapshot.direct_option_counts["/dig"], 22);
        assert_eq!(snapshot.direct_option_counts["/player"], 8);
        assert_eq!(snapshot.direct_option_counts["/player lobby"], 2);
        assert_eq!(snapshot.direct_option_counts["/survey"], 9);
        assert_eq!(snapshot.direct_option_counts["/survey question"], 4);
    }

    #[test]
    fn production_registry_registers_expected_bot_commands() {
        let registry = fixture_registry(true, true);
        let snapshot =
            validate_production_registry(&registry, true).expect("valid command contract");
        for root in REPRESENTATIVE_ROOTS {
            assert!(
                snapshot.top_level_commands.contains(*root),
                "missing {root}"
            );
        }
        assert!(snapshot.top_level_commands.contains("ask"));
        assert_eq!(snapshot.top_level_commands.len(), 45);
    }

    #[test]
    fn production_registry_allows_pet_to_be_disabled_without_a_channel() {
        let registry = fixture_registry(false, false);
        let snapshot =
            validate_production_registry(&registry, false).expect("valid command contract");
        assert!(!snapshot.top_level_commands.contains("pet"));
        assert!(
            snapshot
                .command_paths
                .iter()
                .all(|path| !path.starts_with("/pet "))
        );
        assert_eq!(snapshot.top_level_commands.len(), 43);
    }

    #[test]
    fn validator_recurses_into_scalar_subcommand_options() {
        let mut builder = RegistryBuilder::default();
        let leaf =
            CommandOptionSpec::new("leaf", "Leaf", CommandOptionKind::Subcommand).options(vec![
                CommandOptionSpec::new("value", "Value", CommandOptionKind::String),
            ]);
        builder
            .command(command("example", vec![leaf]))
            .expect("example fixture");
        let snapshot = snapshot_registry(&builder.build());
        assert_eq!(snapshot.direct_option_counts["/example"], 1);
        assert_eq!(snapshot.direct_option_counts["/example leaf"], 1);
        assert!(snapshot.command_paths.contains("/example leaf"));
    }
}
