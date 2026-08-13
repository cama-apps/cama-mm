//! Adapter-neutral command-channel authorization policies.
//!
//! Python still supplies the live Discord and player-service adapters.  Rust
//! preserves fallback ordering, thread-parent handling, private denial, and
//! the historical wrong-channel JC penalty.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GambaChannelCheckContext<'a> {
    pub user_id: i64,
    pub guild_id: Option<i64>,
    pub channel_id: Option<i64>,
    pub channel_name: &'a str,
    pub parent_channel_id: Option<i64>,
    pub parent_channel_name: Option<&'a str>,
    pub extra_allowed_channel_ids: &'a [i64],
}

pub trait GambaChannelPort {
    fn adjust_balance(&mut self, user_id: i64, guild_id: Option<i64>, delta: i64);
    fn send_private_denial(&mut self, content: &str);
}

/// Mirror `commands.checks.require_gamba_channel` without a Discord dependency.
pub fn require_gamba_channel(
    context: GambaChannelCheckContext<'_>,
    port: &mut dyn GambaChannelPort,
) -> bool {
    if context.channel_name.to_lowercase().contains("gamba")
        || context
            .parent_channel_name
            .is_some_and(|name| name.to_lowercase().contains("gamba"))
    {
        return true;
    }

    if context
        .channel_id
        .is_some_and(|channel_id| context.extra_allowed_channel_ids.contains(&channel_id))
        || context
            .parent_channel_id
            .is_some_and(|parent_id| context.extra_allowed_channel_ids.contains(&parent_id))
    {
        return true;
    }

    port.adjust_balance(context.user_id, context.guild_id, -1);
    port.send_private_denial(
        "The ancient spirits reject your offering... this ground is not consecrated. \
         A single jopacoin dissolves into the ether as penance.",
    );
    false
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelCheckContext {
    pub user_id: i64,
    pub guild_id: Option<i64>,
    pub channel_id: Option<i64>,
    pub parent_channel_id: Option<i64>,
    pub configured_dig_channel_id: Option<i64>,
    pub guild_resolves_configured_channel: bool,
}

pub trait DigChannelPort {
    fn require_gamba_channel(&mut self) -> bool;
    fn adjust_balance(&mut self, user_id: i64, guild_id: i64, delta: i64);
    fn send_private_denial(&mut self, content: &str);
}

/// Mirror `commands.checks.require_dig_channel` without a Discord dependency.
pub fn require_dig_channel(context: ChannelCheckContext, port: &mut dyn DigChannelPort) -> bool {
    let Some(dig_channel_id) = context.configured_dig_channel_id else {
        return port.require_gamba_channel();
    };
    let Some(guild_id) = context.guild_id else {
        return port.require_gamba_channel();
    };
    if !context.guild_resolves_configured_channel {
        return port.require_gamba_channel();
    }
    if context.channel_id == Some(dig_channel_id)
        || context.parent_channel_id == Some(dig_channel_id)
    {
        return true;
    }

    port.adjust_balance(context.user_id, guild_id, -1);
    port.send_private_denial(&format!(
        "The earth here is silent. Your tools belong in <#{dig_channel_id}> — \
         a single jopacoin dissolves into the ether as penance."
    ));
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIG_ID: i64 = 12_345;
    const GUILD_ID: i64 = 99_999;

    #[derive(Default)]
    struct FakePort {
        gamba_result: bool,
        gamba_calls: usize,
        gamba_adjustments: Vec<(i64, Option<i64>, i64)>,
        adjustments: Vec<(i64, i64, i64)>,
        private_denials: Vec<String>,
    }

    impl GambaChannelPort for FakePort {
        fn adjust_balance(&mut self, user_id: i64, guild_id: Option<i64>, delta: i64) {
            self.gamba_adjustments.push((user_id, guild_id, delta));
        }

        fn send_private_denial(&mut self, content: &str) {
            self.private_denials.push(content.to_owned());
        }
    }

    impl DigChannelPort for FakePort {
        fn require_gamba_channel(&mut self) -> bool {
            self.gamba_calls += 1;
            self.gamba_result
        }

        fn adjust_balance(&mut self, user_id: i64, guild_id: i64, delta: i64) {
            self.adjustments.push((user_id, guild_id, delta));
        }

        fn send_private_denial(&mut self, content: &str) {
            self.private_denials.push(content.to_owned());
        }
    }

    fn context(channel_id: i64) -> ChannelCheckContext {
        ChannelCheckContext {
            user_id: 77,
            guild_id: Some(GUILD_ID),
            channel_id: Some(channel_id),
            parent_channel_id: None,
            configured_dig_channel_id: Some(DIG_ID),
            guild_resolves_configured_channel: true,
        }
    }

    fn gamba_context<'a>(
        channel_name: &'a str,
        parent_channel_name: Option<&'a str>,
    ) -> GambaChannelCheckContext<'a> {
        GambaChannelCheckContext {
            user_id: 77,
            guild_id: Some(GUILD_ID),
            channel_id: Some(100),
            channel_name,
            parent_channel_id: parent_channel_name.map(|_| 200),
            parent_channel_name,
            extra_allowed_channel_ids: &[],
        }
    }

    #[test]
    fn test_passes_in_gamba_channel() {
        let mut port = FakePort::default();
        assert!(require_gamba_channel(
            gamba_context("gamba-dota", None),
            &mut port
        ));
        assert!(port.private_denials.is_empty());
    }

    #[test]
    fn test_passes_in_thread_under_gamba_channel() {
        let mut port = FakePort::default();
        assert!(require_gamba_channel(
            gamba_context("Market #5", Some("gamba")),
            &mut port
        ));
        assert!(port.private_denials.is_empty());
    }

    #[test]
    fn test_rejects_in_non_gamba_channel() {
        let mut port = FakePort::default();
        assert!(!require_gamba_channel(
            gamba_context("general", None),
            &mut port
        ));
        assert_eq!(port.gamba_adjustments, [(77, Some(GUILD_ID), -1)]);
        assert_eq!(port.private_denials.len(), 1);
    }

    #[test]
    fn test_rejects_in_thread_under_non_gamba_channel() {
        let mut port = FakePort::default();
        assert!(!require_gamba_channel(
            gamba_context("some-thread", Some("general")),
            &mut port
        ));
        assert_eq!(port.gamba_adjustments, [(77, Some(GUILD_ID), -1)]);
        assert_eq!(port.private_denials.len(), 1);
    }

    #[test]
    fn test_handles_missing_parent_gracefully() {
        let mut port = FakePort::default();
        assert!(!require_gamba_channel(
            gamba_context("general", None),
            &mut port
        ));
    }

    #[test]
    fn test_passes_when_channel_id_in_allowed_list() {
        let mut port = FakePort::default();
        let allowed = [12_345];
        let mut request = gamba_context("the-pit", None);
        request.channel_id = Some(12_345);
        request.extra_allowed_channel_ids = &allowed;
        assert!(require_gamba_channel(request, &mut port));
        assert!(port.private_denials.is_empty());
    }

    #[test]
    fn test_passes_when_thread_parent_id_in_allowed_list() {
        let mut port = FakePort::default();
        let allowed = [12_345];
        let mut request = gamba_context("random-thread", Some("the-pit"));
        request.parent_channel_id = Some(12_345);
        request.extra_allowed_channel_ids = &allowed;
        assert!(require_gamba_channel(request, &mut port));
        assert!(port.private_denials.is_empty());
    }

    #[test]
    fn test_extra_ids_not_matching_falls_through_to_reject() {
        let mut port = FakePort::default();
        let allowed = [12_345];
        let mut request = gamba_context("general", None);
        request.channel_id = Some(999);
        request.extra_allowed_channel_ids = &allowed;
        assert!(!require_gamba_channel(request, &mut port));
        assert_eq!(port.private_denials.len(), 1);
    }

    #[test]
    fn test_allowed_exact_name() {
        let mut port = FakePort::default();
        assert!(require_gamba_channel(
            gamba_context("gamba", None),
            &mut port
        ));
    }

    #[test]
    fn test_allowed_substring() {
        let mut port = FakePort::default();
        assert!(require_gamba_channel(
            gamba_context("gamba-zone", None),
            &mut port
        ));
    }

    #[test]
    fn test_allowed_contains_gamba() {
        let mut port = FakePort::default();
        assert!(require_gamba_channel(
            gamba_context("the-gamba", None),
            &mut port
        ));
    }

    #[test]
    fn test_allowed_case_insensitive() {
        let mut port = FakePort::default();
        assert!(require_gamba_channel(
            gamba_context("Gamba-Zone", None),
            &mut port
        ));
    }

    #[test]
    fn test_gamba_blocked_wrong_channel() {
        let mut port = FakePort::default();
        assert!(!require_gamba_channel(
            gamba_context("general", None),
            &mut port
        ));
        assert_eq!(port.gamba_adjustments, [(77, Some(GUILD_ID), -1)]);
        assert!(port.private_denials[0].contains("ancient spirits"));
    }

    #[test]
    fn test_blocked_dm_no_channel_name() {
        let mut port = FakePort::default();
        let mut request = gamba_context("", None);
        request.guild_id = None;
        request.channel_id = None;
        assert!(!require_gamba_channel(request, &mut port));
        assert_eq!(port.gamba_adjustments, [(77, None, -1)]);
    }

    #[test]
    fn test_blocked_empty_channel_name() {
        let mut port = FakePort::default();
        assert!(!require_gamba_channel(gamba_context("", None), &mut port));
        assert_eq!(port.gamba_adjustments, [(77, Some(GUILD_ID), -1)]);
    }

    #[test]
    fn test_falls_back_to_gamba_when_unset() {
        let mut port = FakePort {
            gamba_result: true,
            ..FakePort::default()
        };
        let mut request = context(DIG_ID);
        request.configured_dig_channel_id = None;
        assert!(require_dig_channel(request, &mut port));
        assert_eq!(port.gamba_calls, 1);
    }

    #[test]
    fn test_falls_back_when_channel_unresolvable() {
        let mut port = FakePort::default();
        let mut request = context(DIG_ID);
        request.guild_resolves_configured_channel = false;
        assert!(!require_dig_channel(request, &mut port));
        assert_eq!(port.gamba_calls, 1);
    }

    #[test]
    fn test_falls_back_in_dm() {
        let mut port = FakePort::default();
        let mut request = context(DIG_ID);
        request.guild_id = None;
        assert!(!require_dig_channel(request, &mut port));
        assert_eq!(port.gamba_calls, 1);
    }

    #[test]
    fn test_allowed_in_dig_channel() {
        let mut port = FakePort::default();
        assert!(require_dig_channel(context(DIG_ID), &mut port));
        assert!(port.adjustments.is_empty());
        assert!(port.private_denials.is_empty());
    }

    #[test]
    fn test_require_dig_channel_passes_in_dig_channel() {
        let mut port = FakePort::default();
        assert!(require_dig_channel(context(DIG_ID), &mut port));
        assert!(port.adjustments.is_empty());
        assert!(port.private_denials.is_empty());
    }

    #[test]
    fn test_allowed_in_thread_under_dig() {
        let mut port = FakePort::default();
        let mut request = context(555);
        request.parent_channel_id = Some(DIG_ID);
        assert!(require_dig_channel(request, &mut port));
        assert!(port.adjustments.is_empty());
    }

    #[test]
    fn test_blocked_wrong_channel() {
        let mut port = FakePort::default();
        assert!(!require_dig_channel(context(42), &mut port));
        assert_eq!(port.adjustments, [(77, GUILD_ID, -1)]);
        assert_eq!(port.private_denials.len(), 1);
        assert!(port.private_denials[0].contains("<#12345>"));
    }

    #[test]
    fn test_require_dig_channel_rejects_unrelated_channel() {
        let mut port = FakePort::default();
        assert!(!require_dig_channel(context(42), &mut port));
        assert_eq!(port.adjustments, [(77, GUILD_ID, -1)]);
        assert_eq!(
            port.private_denials,
            [
                "The earth here is silent. Your tools belong in <#12345> — a single \
              jopacoin dissolves into the ether as penance."
            ]
        );
    }

    #[test]
    fn test_require_dig_channel_with_no_env_falls_back_to_gamba() {
        let mut port = FakePort {
            gamba_result: true,
            ..FakePort::default()
        };
        let mut request = context(DIG_ID);
        request.configured_dig_channel_id = None;
        assert!(require_dig_channel(request, &mut port));
        assert_eq!(port.gamba_calls, 1);
        assert!(port.adjustments.is_empty());
    }

    #[test]
    fn test_blocked_thread_under_wrong_parent() {
        let mut port = FakePort::default();
        let mut request = context(555);
        request.parent_channel_id = Some(9_999);
        assert!(!require_dig_channel(request, &mut port));
        assert_eq!(port.adjustments, [(77, GUILD_ID, -1)]);
    }

    #[test]
    fn fallback_paths_never_apply_the_wrong_channel_penalty() {
        let mut port = FakePort::default();
        let mut request = context(42);
        request.configured_dig_channel_id = None;
        assert!(!require_dig_channel(request, &mut port));
        assert!(port.adjustments.is_empty());
        assert!(port.private_denials.is_empty());
    }
}
