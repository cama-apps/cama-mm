use super::*;
use cama_app::match_discovery::{DiscoveryResult, DiscoveryStatus, InternalMatchId, ValveMatchId};
use cama_app::pet_assets::{RasterImage, Rgba};

struct RecapFlow {
    fixture: MatchRuntimeFixture,
    discord: Arc<PublicationDiscord>,
    pending_id: i64,
    archive_root: PathBuf,
    manifest: PathBuf,
    now: i64,
}

impl Drop for RecapFlow {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.archive_root);
    }
}

impl RecapFlow {
    async fn new(lose_reply: bool) -> Self {
        let fixture = MatchRuntimeFixture::new_durable();
        let now = unix_seconds();
        let pending = fixture.pending(now - 1);
        let recorded = fixture
            .provider
            .handler
            .record_match_blocking(&pending, "radiant", None)
            .expect("commit canonical match while cleanup is pending");
        Connection::open(fixture.database.path())
            .expect("open recorded recap fixture")
            .execute(
                "UPDATE matches SET valve_match_id=?1 WHERE match_id=?2 AND guild_id=?3",
                params![92_001, recorded.match_id, GUILD],
            )
            .expect("link canonical Valve match");
        let archive_root = fixture.database.path().with_extension("spectator-recaps");
        let manifest = archive_root
            .join(format!("{GUILD}-{}", pending.pending_match_id))
            .join("manifest.json");
        for (clock, color) in [(0, Rgba(8, 64, 16, 255)), (15, Rgba(64, 8, 16, 255))] {
            let png = RasterImage::new(1240, 704, color).encode_png();
            crate::dota_spectator_recap::capture(
                fixture.database.path().to_owned(),
                GUILD,
                pending.pending_match_id,
                92_001,
                clock,
                png,
                now,
            )
            .await
            .expect("capture qualified screenshot");
        }
        let discord = Arc::new(PublicationDiscord {
            recap_delivery: true,
            recap_manifest: Some(manifest.clone()),
            lose_recap_reply: AtomicBool::new(lose_reply),
            ..Default::default()
        });
        let discovery = Arc::new(StaticRecordedDiscovery {
            outcome: RecordedMatchDiscoveryOutcome::Discovered {
                result: DiscoveryResult {
                    match_id: InternalMatchId(recorded.match_id),
                    status: DiscoveryStatus::Discovered,
                    valve_match_id: Some(ValveMatchId(92_001)),
                    confidence: Some(1.0),
                    player_count: 10,
                    total_players: 10,
                    players_with_steam_id: 10,
                    validation_error: None,
                },
                response: InteractionResponse::message("Recorded match summary")
                    .embed(InteractionEmbed::titled("Enriched match")),
            },
        });
        MatchHandler::run_recorded_match_discovery(
            discovery,
            discord.clone(),
            Some(77_001),
            GUILD,
            recorded.match_id,
            Some((fixture.database.path().to_owned(), pending.pending_match_id)),
        )
        .await;
        Self {
            fixture,
            discord,
            pending_id: pending.pending_match_id,
            archive_root,
            manifest,
            now: unix_seconds() + 1,
        }
    }

    fn finish_cleanup(&self) {
        assert!(
            PendingMatchRepository::new(self.fixture.database.path())
                .delete_pending_match(GUILD, self.pending_id)
                .expect("complete canonical pending cleanup")
        );
    }

    async fn tick(&self, offset: i64) {
        crate::dota_spectator_recap::tick(
            self.fixture.database.path(),
            &[GUILD],
            self.discord.as_ref(),
            self.now + offset,
        )
        .await
        .expect("optional recap worker tick");
    }

    fn saved(&self) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(&self.manifest).expect("read recap checkpoint"))
            .expect("parse recap checkpoint")
    }
}

#[tokio::test]
async fn recorded_spectator_recap_posts_once_after_summary_and_pending_cleanup() {
    let flow = RecapFlow::new(false).await;
    assert_eq!(flow.discord.sent.lock().unwrap().len(), 1);
    flow.tick(0).await;
    assert_eq!(flow.discord.sent.lock().unwrap().len(), 1);
    assert_eq!(flow.discord.recap_attempts.load(Ordering::SeqCst), 0);

    flow.finish_cleanup();
    flow.tick(61).await;
    {
        let sent = flow.discord.sent.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].1.response.content, "Recorded match summary");
        assert!(sent[0].1.response.attachments.is_empty());
        assert_eq!(sent[1].0, sent[0].0);
        let attachment = &sent[1].1.response.attachments[0];
        assert!(attachment.filename.ends_with("-recap.gif"));
        assert!(attachment.bytes.starts_with(b"GIF89a"));
        assert_eq!(sent[1].1.allowed_mentions, DiscordAllowedMentions::None);
    }
    assert_eq!(flow.saved()["job"]["delivered"], 2);
    flow.tick(122).await;
    assert_eq!(flow.discord.sent.lock().unwrap().len(), 2);
    assert_eq!(flow.discord.recap_attempts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn recorded_spectator_recap_recovers_lost_upload_reply_without_duplicate() {
    let flow = RecapFlow::new(true).await;
    flow.finish_cleanup();
    flow.tick(0).await;
    assert_eq!(flow.discord.sent.lock().unwrap().len(), 2);
    assert!(flow.saved()["job"]["delivered"].is_null());

    // Repeated ticks load the archive afresh, just as a restarted worker does.
    // An inconclusive history lookup must never cause a second upload.
    flow.discord
        .fail_recap_history
        .store(true, Ordering::SeqCst);
    flow.tick(61).await;
    assert_eq!(flow.discord.recap_attempts.load(Ordering::SeqCst), 1);
    assert!(flow.saved()["job"]["delivered"].is_null());
    flow.discord
        .fail_recap_history
        .store(false, Ordering::SeqCst);
    flow.tick(122).await;
    assert_eq!(flow.saved()["job"]["delivered"], 2);
    assert_eq!(flow.discord.sent.lock().unwrap().len(), 2);
    assert_eq!(flow.discord.recap_attempts.load(Ordering::SeqCst), 1);
    flow.tick(183).await;
    assert_eq!(flow.discord.sent.lock().unwrap().len(), 2);
}
