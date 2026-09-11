use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use steam_vent::auth::{
    AuthConfirmationHandler, ConsoleAuthConfirmationHandler, DeviceConfirmationHandler,
    EitherConfirmationHandler, GuardDataStore, SharedSecretAuthConfirmationHandler,
    UserProvidedAuthConfirmationHandler,
};
use steam_vent::{Connection, ConnectionError, ServerList};
use thiserror::Error;
use tracing::{debug, warn};

/// Credentials and persistence paths used by the Steam connection.
///
/// `password` and `guard_code` are deliberately optional.  A deployed bot
/// should normally have a persisted refresh token and machine token.  The
/// password is only needed for the first bootstrap or after Steam revokes the
/// session.  `shared_secret` is the base64 Steam Guard TOTP secret and is
/// optional when the first login is confirmed from the mobile app or by the
/// interactive bootstrap helper.
#[derive(Clone)]
pub struct SteamAuthConfig {
    pub username: String,
    pub password: Option<String>,
    pub session_path: PathBuf,
    pub machine_token_path: Option<PathBuf>,
    pub shared_secret: Option<String>,
    pub guard_code: Option<String>,
    pub confirmation: GuardConfirmation,
}

impl Debug for SteamAuthConfig {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SteamAuthConfig")
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("session_path", &self.session_path)
            .field("machine_token_path", &self.machine_token_path)
            .field(
                "shared_secret",
                &self.shared_secret.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "guard_code",
                &self.guard_code.as_ref().map(|_| "<redacted>"),
            )
            .field("confirmation", &self.confirmation)
            .finish()
    }
}

impl SteamAuthConfig {
    /// Construct config for a non-interactive deployed bot.
    pub fn new(username: impl Into<String>, session_path: impl Into<PathBuf>) -> Self {
        Self {
            username: username.into(),
            password: None,
            session_path: session_path.into(),
            machine_token_path: None,
            shared_secret: None,
            guard_code: None,
            confirmation: GuardConfirmation::Device,
        }
    }

    pub fn with_password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    pub fn with_machine_token_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.machine_token_path = Some(path.into());
        self
    }

    pub fn with_shared_secret(mut self, secret: impl Into<String>) -> Self {
        self.shared_secret = Some(secret.into());
        self
    }

    /// Supply a one-time email/device code for a bootstrap login.
    pub fn with_guard_code(mut self, code: impl Into<String>) -> Self {
        self.guard_code = Some(code.into());
        self
    }

    pub fn with_confirmation(mut self, confirmation: GuardConfirmation) -> Self {
        self.confirmation = confirmation;
        self
    }

    pub fn default_machine_token_path(&self) -> PathBuf {
        self.machine_token_path
            .clone()
            .unwrap_or_else(|| self.session_path.with_file_name("machine_tokens.json"))
    }
}

/// How a first login handles Steam Guard.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuardConfirmation {
    /// Generate a TOTP code from `shared_secret`, falling back to mobile
    /// confirmation for confirmation-style challenges.
    Device,
    /// Ask for a code on stdin.  This is suitable for the bootstrap command.
    Console,
}

/// The persisted token envelope.  The value stored in `refresh_token` is the
/// Steam token that `steam-vent` expects as its access argument (Steam's
/// current protocol accepts the refresh token in that field).
#[derive(Clone, Deserialize, Serialize)]
struct SessionFile {
    account: String,
    refresh_token: String,
}

/// A machine-token store with the same JSON shape as steam-vent's built-in
/// store, but with private atomic writes.  Steam Guard machine tokens are
/// bearer credentials and must not be exposed through a default 0644 file or a
/// predictable temporary file.
struct SecureFileGuardDataStore {
    path: PathBuf,
}

impl SecureFileGuardDataStore {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn all_tokens(&self) -> Result<HashMap<String, String>, GuardStoreError> {
        let raw = match read_private_text(&self.path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(source) => {
                return Err(GuardStoreError::Io {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        serde_json::from_str(&raw).map_err(|source| GuardStoreError::Json {
            path: self.path.clone(),
            source,
        })
    }

    fn save(&self, tokens: HashMap<String, String>) -> Result<(), GuardStoreError> {
        ensure_private_parent(&self.path).map_err(|source| GuardStoreError::Io {
            path: self.path.clone(),
            source,
        })?;
        let raw = serde_json::to_vec(&tokens).map_err(|source| GuardStoreError::Json {
            path: self.path.clone(),
            source,
        })?;
        write_private_file(&self.path, &raw).map_err(|source| GuardStoreError::Io {
            path: self.path.clone(),
            source,
        })
    }
}

impl GuardDataStore for SecureFileGuardDataStore {
    type Err = GuardStoreError;

    async fn store(&mut self, account: &str, machine_token: String) -> Result<(), Self::Err> {
        if machine_token.is_empty() {
            return Ok(());
        }
        let mut tokens = self.all_tokens()?;
        tokens.insert(account.to_owned(), machine_token);
        self.save(tokens)
    }

    async fn load(&mut self, account: &str) -> Result<Option<String>, Self::Err> {
        Ok(self
            .all_tokens()?
            .remove(account)
            .filter(|token| !token.is_empty()))
    }
}

#[derive(Debug, Error)]
enum GuardStoreError {
    #[error("could not access machine token file {}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("could not decode machine token file {}: {source}", path.display())]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
}

impl Debug for SessionFile {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionFile")
            .field("account", &self.account)
            .field("refresh_token", &"<redacted>")
            .finish()
    }
}

/// Redacted metadata about the authenticated account and persisted session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamSession {
    pub username: String,
    pub steam_id: u64,
    pub session_path: PathBuf,
}

/// An authenticated Steam connection.  Keep the underlying connection inside
/// this wrapper so callers cannot accidentally print or serialize its token.
pub struct AuthenticatedSteam {
    pub(crate) connection: Connection,
    pub session: SteamSession,
}

impl Debug for AuthenticatedSteam {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthenticatedSteam")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

/// Steam authentication and refresh-token persistence.
pub struct SteamAuth;

impl SteamAuth {
    /// Connect using the persisted token when possible, then perform a fresh
    /// session only when the persisted token is valid.
    ///
    /// This method is safe to call from an unattended runtime: it never asks
    /// for a password or waits for a Steam Guard prompt.  Use
    /// [`Self::bootstrap_login`] for the explicit first-login operation.
    pub async fn connect(config: &SteamAuthConfig) -> Result<AuthenticatedSteam, AuthError> {
        reject_key_logging()?;
        let servers = ServerList::discover().await?;

        if let Some(session) = read_session(&config.session_path)?
            && session.account == config.username
        {
            debug!(account = %config.username, "trying persisted Steam session");
            match Connection::access(&servers, &config.username, &session.refresh_token).await {
                Ok(connection) => {
                    return Ok(AuthenticatedSteam {
                        session: SteamSession {
                            username: config.username.clone(),
                            steam_id: connection.steam_id().into(),
                            session_path: config.session_path.clone(),
                        },
                        connection,
                    });
                }
                Err(error) => {
                    warn!(error = %redact_auth_error(&error), "persisted Steam session rejected; requesting reauthentication");
                }
            }
        }

        Err(AuthError::ReauthenticationRequired {
            session_path: config.session_path.clone(),
            reason: "the persisted Steam session was absent or rejected; run bootstrap_login",
        })
    }

    /// Explicit name for the first-login path used by a CLI or deployment
    /// bootstrap command.  It uses the same secure persistence and MFA flow as
    /// [`Self::connect`], while making the operator intent clear.
    pub async fn bootstrap_login(
        config: &SteamAuthConfig,
    ) -> Result<AuthenticatedSteam, AuthError> {
        reject_key_logging()?;
        let password =
            config
                .password
                .as_deref()
                .ok_or_else(|| AuthError::ReauthenticationRequired {
                    session_path: config.session_path.clone(),
                    reason: "bootstrap_login requires a Steam password",
                })?;
        let servers = ServerList::discover().await?;
        let machine_store = SecureFileGuardDataStore::new(config.default_machine_token_path());
        let handler = confirmation_handler(config).await;
        let connection =
            Connection::login(&servers, &config.username, password, machine_store, handler)
                .await
                .map_err(AuthError::Login)?;
        let refresh_token = connection
            .access_token()
            .ok_or(AuthError::MissingRefreshToken)?;
        write_session(
            &config.session_path,
            &SessionFile {
                account: config.username.clone(),
                refresh_token: refresh_token.to_owned(),
            },
        )?;
        Ok(AuthenticatedSteam {
            session: SteamSession {
                username: config.username.clone(),
                steam_id: connection.steam_id().into(),
                session_path: config.session_path.clone(),
            },
            connection,
        })
    }
}

fn reject_key_logging() -> Result<(), AuthError> {
    if std::env::var_os("SSLKEYLOGFILE").is_some() {
        return Err(AuthError::KeyLoggingEnabled);
    }
    Ok(())
}

async fn confirmation_handler(config: &SteamAuthConfig) -> ConfirmationHandler {
    // A shared secret can answer device/email code challenges.  The device
    // handler runs in parallel for app confirmations.  For console bootstrap,
    // the optional explicit guard code takes precedence in a small handler
    // below; otherwise steam-vent's safe stdin handler is used.
    if let Some(code) = config.guard_code.as_deref() {
        let (mut writer, reader) = tokio::io::duplex(128);
        let code = format!("{code}\n");
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let _ = writer.write_all(code.as_bytes()).await;
        });
        ConfirmationHandler::Code(UserProvidedAuthConfirmationHandler::new(
            reader,
            tokio::io::sink(),
        ))
    } else if let Some(secret) = config.shared_secret.as_deref() {
        ConfirmationHandler::Shared(
            SharedSecretAuthConfirmationHandler::new(secret).or(DeviceConfirmationHandler),
        )
    } else {
        match config.confirmation {
            GuardConfirmation::Console => ConfirmationHandler::Console(
                ConsoleAuthConfirmationHandler::default().or(DeviceConfirmationHandler),
            ),
            GuardConfirmation::Device => ConfirmationHandler::Device(
                DeviceConfirmationHandler.or(ConsoleAuthConfirmationHandler::default()),
            ),
        }
    }
}

enum ConfirmationHandler {
    Shared(
        EitherConfirmationHandler<SharedSecretAuthConfirmationHandler, DeviceConfirmationHandler>,
    ),
    Console(EitherConfirmationHandler<ConsoleAuthConfirmationHandler, DeviceConfirmationHandler>),
    Device(EitherConfirmationHandler<DeviceConfirmationHandler, ConsoleAuthConfirmationHandler>),
    Code(UserProvidedAuthConfirmationHandler<tokio::io::DuplexStream, tokio::io::Sink>),
}

impl AuthConfirmationHandler for ConfirmationHandler {
    async fn handle_confirmation(
        self,
        allowed_confirmations: &[steam_vent::auth::ConfirmationMethod],
    ) -> Option<steam_vent::auth::ConfirmationAction> {
        match self {
            Self::Shared(handler) => handler.handle_confirmation(allowed_confirmations).await,
            Self::Console(handler) => handler.handle_confirmation(allowed_confirmations).await,
            Self::Device(handler) => handler.handle_confirmation(allowed_confirmations).await,
            Self::Code(handler) => handler.handle_confirmation(allowed_confirmations).await,
        }
    }
}

fn read_session(path: &Path) -> Result<Option<SessionFile>, AuthError> {
    match read_private_text(path) {
        Ok(raw) => Ok(Some(serde_json::from_str(&raw).map_err(|source| {
            AuthError::SessionFile {
                path: path.to_owned(),
                source,
            }
        })?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(AuthError::SessionIo {
            path: path.to_owned(),
            source,
        }),
    }
}

fn write_session(path: &Path, session: &SessionFile) -> Result<(), AuthError> {
    let raw = serde_json::to_vec(session).map_err(|source| AuthError::SessionSerialize {
        path: path.to_owned(),
        source,
    })?;
    write_private_file(path, &raw).map_err(|source| AuthError::SessionIo {
        path: path.to_owned(),
        source,
    })
}

fn read_private_text(path: &Path) -> io::Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "credential file must not be a symbolic link",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "credential file permissions must be private",
            ));
        }
    }
    fs::read_to_string(path)
}

fn ensure_private_parent(path: &Path) -> io::Result<()> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    let existed = fs::symlink_metadata(parent).is_ok();
    fs::create_dir_all(parent)?;
    if !existed {
        set_private_directory_permissions(parent)?;
    }
    Ok(())
}

fn write_private_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    ensure_private_parent(path)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("credentials");
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);

    for attempt in 0..16_u64 {
        let temporary = parent.join(format!(
            ".{file_name}.tmp-{}-{timestamp}-{}-{}.new",
            std::process::id(),
            sequence,
            attempt,
        ));
        let mut file = match open_private_temp(&temporary) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let result = (|| {
            file.write_all(contents)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        return result;
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a private credential temporary file",
    ))
}

fn open_private_temp(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn redact_auth_error(error: &ConnectionError) -> String {
    // steam-vent's errors currently contain no credentials, but keeping this
    // boundary explicit prevents a future error display from being logged as
    // part of a token-bearing connection failure.
    match error {
        ConnectionError::LoginError(login) => format!("login failed: {login}"),
        ConnectionError::AccessToken(_) => "persisted token rejected".to_owned(),
        ConnectionError::Network(_) => "network error".to_owned(),
        ConnectionError::Aborted => "authentication aborted".to_owned(),
        ConnectionError::UnsupportedConfirmationAction(_) => {
            "unsupported Steam Guard confirmation".to_owned()
        }
        _ => "Steam authentication error".to_owned(),
    }
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("SSLKEYLOGFILE must be unset before connecting to Steam")]
    KeyLoggingEnabled,
    #[error("Steam server discovery failed: {0}")]
    Discovery(#[from] steam_vent::ServerDiscoveryError),
    #[error("Steam login failed: {0}")]
    Login(#[source] ConnectionError),
    #[error(
        "Steam session needs reauthentication ({reason}); provide a password and run bootstrap_login; session file: {session_path}"
    )]
    ReauthenticationRequired {
        session_path: PathBuf,
        reason: &'static str,
    },
    #[error("Steam returned no refresh token")]
    MissingRefreshToken,
    #[error("could not read Steam session file {}: {source}", path.display())]
    SessionIo { path: PathBuf, source: io::Error },
    #[error("could not decode Steam session file {}: {source}", path.display())]
    SessionFile {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("could not encode Steam session file {}: {source}", path.display())]
    SessionSerialize {
        path: PathBuf,
        source: serde_json::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_config_debug_redacts_secrets() {
        let config = SteamAuthConfig::new("bot", "/tmp/cama-steam-session.json")
            .with_password("password")
            .with_shared_secret("shared-secret")
            .with_guard_code("12345");
        let debug = format!("{config:?}");
        assert!(!debug.contains(": Some(\"password\")"));
        assert!(!debug.contains(": Some(\"shared-secret\")"));
        assert!(!debug.contains(": Some(\"12345\")"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn session_round_trip_is_json_and_redacted() {
        let path = tempfile_path("cama-steam-auth");
        let file = SessionFile {
            account: "bot".to_owned(),
            refresh_token: "token-value".to_owned(),
        };
        write_session(&path, &file).expect("write session");
        let loaded = read_session(&path).expect("read session").expect("session");
        assert_eq!(loaded.account, "bot");
        assert_eq!(loaded.refresh_token, "token-value");
        assert!(!format!("{loaded:?}").contains("token-value"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path)
                    .expect("session metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let _ = fs::remove_file(path);
    }

    fn tempfile_path(prefix: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let entropy = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        path.push(format!("{prefix}-{}-{entropy}.json", std::process::id()));
        path
    }
}
