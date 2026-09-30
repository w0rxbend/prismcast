//! Authentication configuration for remote servers (IPC-001; PLAN.md §24).
//!
//! The Unix socket's primary access control is **filesystem permissions**
//! (ADR-0006 §5: the socket lives in `$XDG_RUNTIME_DIR/prismcast/` with
//! `0700`/`0600` modes, so only the owning user can connect). The default
//! local configuration therefore grants every session full
//! [`Permission::Admin`] rights without a handshake credential — this is the
//! documented "allow-local" policy.
//!
//! A bearer token can additionally be required, either injected by the
//! embedder ([`AuthConfig::token`]) or read from
//! `$XDG_CONFIG_HOME/prismcast/remote.toml`:
//!
//! ```toml
//! token = "shared-secret"
//! permissions = ["read", "control_scenes"]
//! ```
//!
//! The file is parsed with a small built-in parser covering exactly the two
//! keys above (comments, sections, quoted strings, and string arrays); it is
//! an interim solution until the workspace adopts a TOML crate. When the
//! file is absent the allow-local policy applies; when it is malformed the
//! server falls back to allow-local with a warning (the socket's filesystem
//! permissions still gate access).

use std::path::{Path, PathBuf};

use prismcast_protocol::handshake::{AuthResponse, Permission};

/// How a server authenticates incoming sessions and which permissions it
/// grants them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthConfig {
    /// Local-trust policy: no credential required; every session receives the
    /// configured permissions (full `Admin` by default). Appropriate for the
    /// Unix socket, where filesystem permissions already restrict access to
    /// the owning user (ADR-0006 §5).
    AllowLocal {
        /// Permissions granted to every session.
        permissions: Vec<Permission>,
    },
    /// Bearer token required in `Identify` (`AuthResponse::Token`); sessions
    /// presenting the correct token receive the configured permissions.
    Token {
        /// The expected token.
        token: String,
        /// Permissions granted on success.
        permissions: Vec<Permission>,
    },
}

impl AuthConfig {
    /// The default local policy: no credential, full [`Permission::Admin`].
    pub fn allow_local() -> Self {
        Self::AllowLocal {
            permissions: vec![Permission::Admin],
        }
    }

    /// Requires `token` and grants `permissions` on success.
    pub fn token(token: impl Into<String>, permissions: Vec<Permission>) -> Self {
        Self::Token {
            token: token.into(),
            permissions,
        }
    }

    /// Loads the configuration from an explicit `remote.toml` path.
    pub fn from_file(path: &Path) -> Result<Self, AuthError> {
        let content = std::fs::read_to_string(path).map_err(AuthError::Io)?;
        parse_remote_toml(&content)
    }

    /// Loads [`default_config_path`] if it exists; otherwise (or on a parse
    /// failure, with a warning logged) returns [`AuthConfig::allow_local`].
    pub fn load_default() -> Self {
        let path = default_config_path();
        if !path.is_file() {
            return Self::allow_local();
        }
        match Self::from_file(&path) {
            Ok(config) => config,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "ignoring malformed remote.toml");
                Self::allow_local()
            }
        }
    }

    /// The permissions a successfully authenticated session receives.
    pub fn granted_permissions(&self) -> &[Permission] {
        match self {
            Self::AllowLocal { permissions } | Self::Token { permissions, .. } => permissions,
        }
    }

    /// Verifies the `Identify` authentication field, returning the granted
    /// permissions or `None` when authentication fails.
    pub(crate) fn authenticate(&self, auth: Option<&AuthResponse>) -> Option<Vec<Permission>> {
        match self {
            Self::AllowLocal { permissions } => Some(permissions.clone()),
            Self::Token { token, permissions } => match auth {
                Some(AuthResponse::Token { token: presented }) if presented == token => {
                    Some(permissions.clone())
                }
                // Challenge-response is not offered by this server (no
                // challenge in `Hello`), so a challenge answer is a failure.
                _ => None,
            },
        }
    }
}

/// The default auth configuration file:
/// `$XDG_CONFIG_HOME/prismcast/remote.toml` (falling back to
/// `~/.config/prismcast/remote.toml` when `XDG_CONFIG_HOME` is unset).
pub fn default_config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    match base {
        Some(base) => base.join("prismcast").join("remote.toml"),
        None => PathBuf::from("remote.toml"),
    }
}

/// Failures loading or parsing the auth configuration.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The file could not be read.
    #[error("I/O error: {0}")]
    Io(std::io::Error),
    /// A line in the file is not a recognized `key = value` pair.
    #[error("parse error on line {line}: {message}")]
    Parse {
        /// 1-based line number.
        line: usize,
        /// What went wrong.
        message: String,
    },
    /// A permission name in `permissions` is not a known scope.
    #[error("unknown permission: {0}")]
    UnknownPermission(String),
}

/// Parses the interim `remote.toml` subset (see module docs).
fn parse_remote_toml(content: &str) -> Result<AuthConfig, AuthError> {
    let mut token: Option<String> = None;
    let mut permissions: Option<Vec<Permission>> = None;
    for (index, raw_line) in content.lines().enumerate() {
        let line = strip_comment(raw_line).trim().to_string();
        if line.is_empty() || (line.starts_with('[') && line.ends_with(']')) {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(AuthError::Parse {
                line: index + 1,
                message: "expected `key = value`".to_string(),
            });
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "token" => token = Some(parse_string(value, index + 1)?),
            "permissions" => {
                let names = parse_string_array(value, index + 1)?;
                let mut parsed = Vec::with_capacity(names.len());
                for name in names {
                    let permission =
                        serde_json::from_value::<Permission>(serde_json::Value::String(name))
                            .map_err(|e| AuthError::UnknownPermission(e.to_string()))?;
                    parsed.push(permission);
                }
                permissions = Some(parsed);
            }
            other => {
                return Err(AuthError::Parse {
                    line: index + 1,
                    message: format!("unknown key `{other}`"),
                });
            }
        }
    }
    match token {
        Some(token) => Ok(AuthConfig::Token {
            token,
            permissions: permissions.unwrap_or_else(|| vec![Permission::Admin]),
        }),
        None => Ok(AuthConfig::AllowLocal {
            permissions: permissions.unwrap_or_else(|| vec![Permission::Admin]),
        }),
    }
}

/// Removes a `#` comment, honoring double-quoted strings.
fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in line.char_indices() {
        match ch {
            '\\' if in_string => escaped = !escaped,
            '"' if !escaped => in_string = !in_string,
            '#' if !in_string => return &line[..index],
            _ => escaped = false,
        }
    }
    line
}

fn parse_string(value: &str, line: usize) -> Result<String, AuthError> {
    let inner = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .ok_or_else(|| AuthError::Parse {
            line,
            message: "expected a quoted string".to_string(),
        })?;
    Ok(inner.replace("\\\"", "\"").replace("\\\\", "\\"))
}

fn parse_string_array(value: &str, line: usize) -> Result<Vec<String>, AuthError> {
    let inner = value
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .ok_or_else(|| AuthError::Parse {
            line,
            message: "expected an array of quoted strings".to_string(),
        })?;
    let mut items = Vec::new();
    for part in inner.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        items.push(parse_string(part, line)?);
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_local_grants_admin_without_credentials() {
        let config = AuthConfig::allow_local();
        assert_eq!(
            config.authenticate(None),
            Some(vec![Permission::Admin]),
            "no credential needed"
        );
        assert_eq!(config.granted_permissions(), &[Permission::Admin]);
    }

    #[test]
    fn token_auth_accepts_only_the_configured_token() {
        let config = AuthConfig::token("s3cret", vec![Permission::Read]);
        assert_eq!(config.authenticate(None), None);
        assert_eq!(
            config.authenticate(Some(&AuthResponse::Token {
                token: "wrong".into()
            })),
            None
        );
        assert_eq!(
            config.authenticate(Some(&AuthResponse::Challenge {
                response: "x".into()
            })),
            None
        );
        assert_eq!(
            config.authenticate(Some(&AuthResponse::Token {
                token: "s3cret".into()
            })),
            Some(vec![Permission::Read])
        );
    }

    #[test]
    fn parses_token_and_permissions() {
        let config = parse_remote_toml(
            r#"
            # Prismcast remote auth
            [remote]
            token = "abc#def" # trailing comment
            permissions = ["read", "control_scenes"]
            "#,
        )
        .expect("parse");
        assert_eq!(
            config,
            AuthConfig::Token {
                token: "abc#def".into(),
                permissions: vec![Permission::Read, Permission::ControlScenes],
            }
        );
    }

    #[test]
    fn file_without_token_means_allow_local() {
        let config = parse_remote_toml("permissions = [\"read\"]\n").expect("parse");
        assert_eq!(
            config,
            AuthConfig::AllowLocal {
                permissions: vec![Permission::Read],
            }
        );
        assert_eq!(
            parse_remote_toml("").expect("parse"),
            AuthConfig::allow_local()
        );
    }

    #[test]
    fn malformed_lines_and_unknown_permissions_are_errors() {
        assert!(matches!(
            parse_remote_toml("not a key value"),
            Err(AuthError::Parse { line: 1, .. })
        ));
        assert!(matches!(
            parse_remote_toml("token = abc"),
            Err(AuthError::Parse { .. })
        ));
        assert!(matches!(
            parse_remote_toml("permissions = [\"bogus\"]"),
            Err(AuthError::UnknownPermission(_))
        ));
    }
}
