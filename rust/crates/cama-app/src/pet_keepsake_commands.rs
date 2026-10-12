//! Discord-independent keepsake album and diary presentation.

use cama_domain::formatting::escape_discord_text;
use cama_domain::pet_evolution::{PetCalling, calling_profile};
use cama_domain::pet_keepsakes::{
    AlbumEntry, DiaryEntry, DiaryKind, GiftReceipt, KEEPSAKES, get_keepsake,
};

use crate::pet_commands::{Embed, EmbedColor};

pub const DIARY_PAGE_SIZE: i64 = 15;

#[must_use]
pub fn gift_text(gift: &GiftReceipt) -> String {
    let name = escape_discord_text(&gift.pet_name);
    let keepsake = get_keepsake(&gift.keepsake_id);
    format!(
        "**{name}** brought you **{}**!\n{}\nSaved in `/pet album` · Moments in `/pet diary`.",
        keepsake.map_or("a keepsake", |item| item.name),
        keepsake.map_or("A little something to remember them by.", |item| item
            .description),
    )
}

#[must_use]
pub fn build_album_embed(entries: &[AlbumEntry]) -> Embed {
    let discovered = KEEPSAKES
        .iter()
        .filter(|item| entries.iter().any(|entry| entry.keepsake_id == item.id))
        .count();
    let mut embed = Embed::new(
        "🎁 Keepsake album",
        format!(
            "**{discovered}/{} discovered**\nLittle gifts, kept forever. Check on your active, hatched cama with `/pet status` for a daily gift. Adult callings can bring special keepsakes.",
            KEEPSAKES.len()
        ),
        EmbedColor::Gold,
    );
    for item in &KEEPSAKES {
        if let Some(entry) = entries.iter().find(|entry| entry.keepsake_id == item.id) {
            embed.field(
                format!("{} ×{}", item.name, entry.count),
                format!(
                    "{}\nFirst from **{}** · <t:{}:d>",
                    item.description,
                    escape_discord_text(&entry.first_pet_name),
                    entry.first_received_at
                ),
                false,
            );
        }
    }
    if discovered == 0 {
        embed
            .description
            .push_str("\n\nYour first keepsake is still waiting to be found.");
    }
    embed.footer =
        Some("Cosmetic only · One gift per owner per game day · Resets at 12:00 UTC".to_owned());
    embed
}

#[must_use]
pub fn build_diary_embed(entries: &[DiaryEntry], page: i64, has_more: bool) -> Embed {
    let lines = entries
        .iter()
        .map(|entry| {
            let name = escape_discord_text(&entry.pet_name);
            let moment = match &entry.kind {
                DiaryKind::Hatched => "hatched into the world.".to_owned(),
                DiaryKind::Calling(value) => {
                    let calling = PetCalling::ALL
                        .into_iter()
                        .find(|calling| calling.as_str() == value);
                    format!(
                        "found their calling: **{}**.",
                        calling.map_or("a new path", |calling| calling_profile(calling).label)
                    )
                }
                DiaryKind::FirstBrawlWin => "won their first brawl!".to_owned(),
                DiaryKind::Gift(id) => format!(
                    "brought you **{}**.",
                    get_keepsake(id).map_or("a keepsake", |item| item.name)
                ),
            };
            format!("<t:{}:d> · **{name}** {moment}", entry.occurred_at)
        })
        .collect::<Vec<_>>();
    let description = if lines.is_empty() {
        if page == 1 {
            "No memories yet. Your cama's story begins when they hatch."
        } else {
            "No older memories on this page. Use /pet diary page:1 for the latest."
        }
        .to_owned()
    } else {
        lines.join("\n\n")
    };
    let mut embed = Embed::new("📖 Cama diary", description, EmbedColor::Blue);
    embed.footer = Some(if has_more {
        format!(
            "Page {page} · Newest first · Older memories: /pet diary page:{}",
            page + 1
        )
    } else {
        format!("Page {page} · Newest first · Your keepsakes live in /pet album")
    });
    embed
}

#[cfg(test)]
mod tests {
    use super::*;
    use cama_domain::pet_keepsakes::{DiaryKind, KEEPSAKES};

    #[test]
    fn album_shows_discoveries_and_preserves_mystery_of_missing_gifts() {
        let embed = build_album_embed(&[AlbumEntry {
            keepsake_id: "moon_button".to_owned(),
            first_pet_name: "Mochi".to_owned(),
            first_received_at: 100,
            count: 3,
        }]);
        assert!(embed.description.contains("1/22"));
        let found = embed
            .fields
            .iter()
            .find(|field| field.name.contains("Moon Button"))
            .unwrap();
        assert!(found.value.contains("Mochi"));
        assert!(found.value.contains("100"));
        assert!(found.name.contains('3'));
        assert!(!format!("{embed:?}").contains("Acorn Trophy"));
    }

    #[test]
    fn full_album_and_diary_fit_discord_embed_limits() {
        let entries: Vec<_> = KEEPSAKES
            .iter()
            .map(|item| AlbumEntry {
                keepsake_id: item.id.to_owned(),
                first_pet_name: "*".repeat(32),
                first_received_at: i64::MAX,
                count: i64::MAX,
            })
            .collect();
        let album = build_album_embed(&entries);
        assert_eq!(album.fields.len(), 22);
        let diary = build_diary_embed(
            &(0..15)
                .map(|i| DiaryEntry {
                    pet_id: 1,
                    pet_name: "*".repeat(32),
                    occurred_at: i64::MAX - i,
                    kind: DiaryKind::Gift("moon_button".to_owned()),
                })
                .collect::<Vec<_>>(),
            2,
            true,
        );
        for embed in [&album, &diary] {
            assert!(embed.description.chars().count() <= 4096);
            assert!(embed.fields.len() <= 25);
            let mut total = embed.title.chars().count()
                + embed.description.chars().count()
                + embed.footer.as_deref().unwrap_or_default().chars().count();
            for field in &embed.fields {
                assert!(field.value.chars().count() <= 1024);
                total += field.name.chars().count() + field.value.chars().count();
            }
            assert!(total <= 6000, "embed has {total} characters");
        }
        assert!(diary.footer.unwrap().contains("page:3"));
    }

    #[test]
    fn diary_and_gifts_escape_names_and_render_milestones() {
        let entries = vec![
            DiaryEntry {
                pet_id: 1,
                pet_name: "*Mochi* @everyone".to_owned(),
                occurred_at: 100,
                kind: DiaryKind::Hatched,
            },
            DiaryEntry {
                pet_id: 1,
                pet_name: "Mochi".to_owned(),
                occurred_at: 200,
                kind: DiaryKind::Calling("prospector".to_owned()),
            },
            DiaryEntry {
                pet_id: 1,
                pet_name: "Mochi".to_owned(),
                occurred_at: 300,
                kind: DiaryKind::FirstBrawlWin,
            },
        ];
        let embed = build_diary_embed(&entries, 1, false);
        assert!(embed.description.contains("hatched"));
        assert!(embed.description.contains("Prospector"));
        assert!(embed.description.contains("first brawl"));
        assert!(!embed.description.contains("@everyone"));
        assert!(embed.description.contains(r"\*Mochi\*"));
        let gift = gift_text(&GiftReceipt {
            pet_id: 1,
            pet_name: "@everyone".to_owned(),
            keepsake_id: "moon_button".to_owned(),
            received_at: 400,
        });
        assert!(gift.contains("Moon Button"));
        assert!(!gift.contains("@everyone"));
        assert!(gift.contains("/pet album"));
    }
}
