//! Shared primitives for reasoning about `pulsoid_connections` rows.
//!
//! The `connection_state = 'error'` column acts as a sticky terminal signal:
//! once a row is in that state, only a fresh re-auth (OAuth callback or manual
//! token upload) may transition it out. All other writes carry a
//! `WHERE ... AND ($target = 'error' OR connection_state != 'error')` guard
//! so they can't resurrect a dead row.

/// Type-safe representation of the `connection_state` TEXT column in
/// `pulsoid_connections`. Maps to/from the lowercase string literals
/// enforced by the DB CHECK constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ConnectionState {
    Pending,
    Connected,
    Error,
}

impl ConnectionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Connected => "connected",
            Self::Error => "error",
        }
    }
}

impl std::fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
