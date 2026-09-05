//! Node names. A node's name is its *address*: it is what agents type in
//! `wheel msg <name>`, what table nodes derive their sqlite table from, and what
//! appears in injected context headers. It is therefore validated hard.
//!
//! Contract (ARCHITECTURE.md §3): `^[a-z0-9][a-z0-9-_]{0,62}$`, unique per project.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Names the engine refuses to assign to a node because they are already
/// meaningful in the message-delivery contract or the CLI surface.
///
/// `user` is the `from_node` the engine stamps on messages originating in the
/// UI (§3 "Message delivery contract"), so a node called `user` would make
/// `wheel msg user ...` ambiguous. The others are reserved for future
/// engine-internal senders so we never have to break someone's board later.
pub const RESERVED_NAMES: &[&str] = &["user", "wheel", "system", "engine"];

pub const NAME_MAX_LEN: usize = 63;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("name must not be empty")]
    Empty,
    #[error("name must be at most {NAME_MAX_LEN} characters, got {0}")]
    TooLong(usize),
    #[error("name must start with a lowercase letter or digit, got {0:?}")]
    BadFirstChar(char),
    #[error("name may only contain lowercase letters, digits, '-' and '_', got {0:?}")]
    BadChar(char),
    #[error("{0:?} is a reserved name")]
    Reserved(String),
}

/// A validated node name. Construct with [`NodeName::new`]; it is impossible to
/// build an invalid one, including via `serde` (deserialization re-validates).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeName(String);

impl NodeName {
    pub fn new(raw: impl Into<String>) -> Result<Self, NameError> {
        let raw = raw.into();
        validate_name(&raw)?;
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }

    /// The sqlite table backing a `table` node: `t_<name>`.
    ///
    /// Safe to interpolate into SQL *only* because the name charset is
    /// restricted to `[a-z0-9_-]` and `-` is additionally rejected here.
    pub fn sqlite_table(&self) -> Option<String> {
        if self.0.contains('-') {
            None
        } else {
            Some(format!("t_{}", self.0))
        }
    }
}

/// Validate a candidate node name against the §3 contract.
pub fn validate_name(raw: &str) -> Result<(), NameError> {
    let mut chars = raw.chars();
    let first = chars.next().ok_or(NameError::Empty)?;

    // Count in chars, not bytes: a multi-byte char would otherwise report a
    // confusing length. Any non-ASCII char is rejected below anyway.
    let len = raw.chars().count();
    if len > NAME_MAX_LEN {
        return Err(NameError::TooLong(len));
    }

    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(NameError::BadFirstChar(first));
    }
    for c in chars {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' && c != '_' {
            return Err(NameError::BadChar(c));
        }
    }

    if RESERVED_NAMES.contains(&raw) {
        return Err(NameError::Reserved(raw.to_string()));
    }
    Ok(())
}

impl fmt::Display for NodeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for NodeName {
    type Err = NameError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl AsRef<str> for NodeName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Serialize for NodeName {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for NodeName {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for NodeName {
    fn schema_name() -> String {
        "NodeName".into()
    }
    fn json_schema(_: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        use schemars::schema::{InstanceType, SchemaObject, StringValidation};
        SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            string: Some(Box::new(StringValidation {
                max_length: Some(NAME_MAX_LEN as u32),
                min_length: Some(1),
                pattern: Some("^[a-z0-9][a-z0-9-_]{0,62}$".into()),
            })),
            metadata: Some(Box::new(schemars::schema::Metadata {
                description: Some(
                    "A node's unique, addressable name. Reserved: user, wheel, system, engine."
                        .into(),
                ),
                ..Default::default()
            })),
            ..Default::default()
        }
        .into()
    }
}

// ---------------------------------------------------------------------------
// Idents (table column names)
// ---------------------------------------------------------------------------

/// A validated identifier used where we must interpolate into SQL but the
/// *node* reserved-name list does not apply — currently `table` column names.
///
/// Same charset as [`NodeName`] (so quoting into DDL is safe) but `user`,
/// `system` etc. are perfectly reasonable column names, and `-` is rejected
/// outright since these become bare sqlite column identifiers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ident(String);

impl Ident {
    pub fn new(raw: impl Into<String>) -> Result<Self, NameError> {
        let raw = raw.into();
        let mut chars = raw.chars();
        let first = chars.next().ok_or(NameError::Empty)?;
        let len = raw.chars().count();
        if len > NAME_MAX_LEN {
            return Err(NameError::TooLong(len));
        }
        if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
            return Err(NameError::BadFirstChar(first));
        }
        for c in chars {
            if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '_' {
                return Err(NameError::BadChar(c));
            }
        }
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Ident {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for Ident {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Ident {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for Ident {
    fn schema_name() -> String {
        "Ident".into()
    }
    fn json_schema(_: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        use schemars::schema::{InstanceType, Metadata, SchemaObject, StringValidation};
        SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            string: Some(Box::new(StringValidation {
                max_length: Some(NAME_MAX_LEN as u32),
                min_length: Some(1),
                pattern: Some("^[a-z0-9][a-z0-9_]{0,62}$".into()),
            })),
            metadata: Some(Box::new(Metadata {
                description: Some("A sqlite-safe identifier (table column name).".into()),
                ..Default::default()
            })),
            ..Default::default()
        }
        .into()
    }
}
