//! Pure permission policy for privileged bot commands.
//!
//! Discord object lookup stays in the runtime layer. That layer reduces an
//! interaction to [`PermissionContext`], preserving whether an authoritative
//! guild-member permission set was found. The policy here then mirrors the
//! Python precedence exactly: explicit admin allowlist, guild-member
//! permissions, user-object permission fallback, and finally denial.

/// The Discord permission flags used by the bot's privileged commands.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DiscordPermissions {
    /// Discord's Administrator permission.
    pub administrator: bool,
    /// Discord's Manage Server (`manage_guild`) permission.
    pub manage_guild: bool,
}

impl DiscordPermissions {
    /// Whether either flag grants the bot's admin permission.
    #[must_use]
    pub const fn grants_admin(self) -> bool {
        self.administrator || self.manage_guild
    }
}

/// Permission-relevant data extracted from one Discord interaction.
///
/// `guild_member_permissions` is `Some` only when a guild lookup returned a
/// member with an available permission object. If it is `Some`, that result is
/// authoritative and `user_permissions` is not consulted. `None` covers all
/// cases that Python treats alike: no guild, no callable member lookup, no
/// member, or a member without a permission object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PermissionContext {
    /// Discord snowflake for the interaction user.
    pub user_id: u64,
    /// Permissions obtained from the guild's member lookup, when available.
    pub guild_member_permissions: Option<DiscordPermissions>,
    /// Permissions attached directly to the interaction user, when available.
    pub user_permissions: Option<DiscordPermissions>,
}

impl PermissionContext {
    /// Construct an interaction context without either Discord permission
    /// source. Allowlist checks still apply.
    #[must_use]
    pub const fn new(user_id: u64) -> Self {
        Self {
            user_id,
            guild_member_permissions: None,
            user_permissions: None,
        }
    }

    /// Attach the permission set returned by the guild's member lookup.
    #[must_use]
    pub const fn with_guild_member_permissions(mut self, permissions: DiscordPermissions) -> Self {
        self.guild_member_permissions = Some(permissions);
        self
    }

    /// Attach fallback permissions exposed directly by the interaction user.
    #[must_use]
    pub const fn with_user_permissions(mut self, permissions: DiscordPermissions) -> Self {
        self.user_permissions = Some(permissions);
        self
    }
}

/// Check whether the user is explicitly listed in the admin allowlist.
///
/// An empty allowlist grants nobody access.
#[must_use]
pub fn has_allowlisted_admin(interaction: &PermissionContext, admin_user_ids: &[u64]) -> bool {
    admin_user_ids.contains(&interaction.user_id)
}

/// Check whether the user is explicitly listed in the Tax Man allowlist.
#[must_use]
pub fn has_allowlisted_tax_man(interaction: &PermissionContext, tax_man_user_ids: &[u64]) -> bool {
    tax_man_user_ids.contains(&interaction.user_id)
}

/// Check whether an interaction has the bot's admin permission.
///
/// The admin allowlist wins first. An available guild-member permission set is
/// checked next and is authoritative, including a negative result. Only when
/// that set is unavailable do permissions attached to the user object act as a
/// fallback.
#[must_use]
pub fn has_admin_permission(interaction: &PermissionContext, admin_user_ids: &[u64]) -> bool {
    if !admin_user_ids.is_empty() && has_allowlisted_admin(interaction, admin_user_ids) {
        return true;
    }

    if let Some(permissions) = interaction.guild_member_permissions {
        return permissions.grants_admin();
    }

    interaction
        .user_permissions
        .is_some_and(DiscordPermissions::grants_admin)
}

/// Check whether an interaction may use Tax Man audit/enforcement commands.
///
/// Admin permission always grants Tax Man access. The dedicated Tax Man
/// allowlist can additionally grant access without granting admin permission.
#[must_use]
pub fn has_tax_man_permission(
    interaction: &PermissionContext,
    admin_user_ids: &[u64],
    tax_man_user_ids: &[u64],
) -> bool {
    has_admin_permission(interaction, admin_user_ids)
        || has_allowlisted_tax_man(interaction, tax_man_user_ids)
}

#[cfg(test)]
mod tests {
    use super::{
        DiscordPermissions, PermissionContext, has_admin_permission, has_allowlisted_admin,
        has_allowlisted_tax_man, has_tax_man_permission,
    };

    #[test]
    fn test_has_allowlisted_admin() {
        let interaction = PermissionContext::new(101);

        assert!(has_allowlisted_admin(&interaction, &[101]));
    }

    #[test]
    fn test_has_admin_permission_allowlist() {
        let interaction = PermissionContext::new(202);

        assert!(has_admin_permission(&interaction, &[202]));
    }

    #[test]
    fn test_has_admin_permission_guild_member_permissions() {
        let interaction =
            PermissionContext::new(303).with_guild_member_permissions(DiscordPermissions {
                administrator: true,
                manage_guild: false,
            });

        assert!(has_admin_permission(&interaction, &[]));
    }

    #[test]
    fn test_has_admin_permission_user_permissions_fallback() {
        let interaction = PermissionContext::new(404).with_user_permissions(DiscordPermissions {
            administrator: false,
            manage_guild: true,
        });

        assert!(has_admin_permission(&interaction, &[]));
    }

    #[test]
    fn test_has_admin_permission_false() {
        let interaction = PermissionContext::new(505);

        assert!(!has_admin_permission(&interaction, &[]));
    }

    #[test]
    fn test_has_allowlisted_tax_man() {
        let interaction = PermissionContext::new(606);

        assert!(has_allowlisted_tax_man(&interaction, &[606]));
    }

    #[test]
    fn test_has_tax_man_permission_defaults_to_admin() {
        let interaction = PermissionContext::new(707);

        assert!(has_tax_man_permission(&interaction, &[707], &[]));
    }

    #[test]
    fn test_has_tax_man_permission_allowlist() {
        let interaction = PermissionContext::new(808);

        assert!(has_tax_man_permission(&interaction, &[], &[808]));
    }

    #[test]
    fn test_has_tax_man_permission_false() {
        let interaction = PermissionContext::new(909);

        assert!(!has_tax_man_permission(&interaction, &[], &[]));
    }

    #[test]
    fn guild_member_permissions_are_authoritative_over_user_fallback() {
        let interaction = PermissionContext::new(1)
            .with_guild_member_permissions(DiscordPermissions::default())
            .with_user_permissions(DiscordPermissions {
                administrator: true,
                manage_guild: true,
            });

        assert!(!has_admin_permission(&interaction, &[]));
    }
}
