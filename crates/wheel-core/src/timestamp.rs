//! RFC3339 UTC timestamps (ARCHITECTURE.md §2: "Time: RFC3339 UTC").

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

/// An instant, always serialized as an RFC3339 UTC string, e.g.
/// `2026-09-05T00:21:00Z`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(OffsetDateTime);

impl Timestamp {
    pub fn now() -> Self {
        Self(OffsetDateTime::now_utc())
    }
    pub fn into_inner(self) -> OffsetDateTime {
        self.0
    }
    pub fn to_rfc3339(self) -> String {
        self.0
            .to_offset(time::UtcOffset::UTC)
            .format(&Rfc3339)
            .expect("RFC3339 formatting of a UTC timestamp cannot fail")
    }
    pub fn parse_rfc3339(s: &str) -> Result<Self, time::error::Parse> {
        Ok(Self(OffsetDateTime::parse(s, &Rfc3339)?))
    }
}

impl From<OffsetDateTime> for Timestamp {
    fn from(t: OffsetDateTime) -> Self {
        Self(t)
    }
}

impl std::fmt::Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_rfc3339())
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_rfc3339())
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::parse_rfc3339(&raw).map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for Timestamp {
    fn schema_name() -> String {
        "Timestamp".into()
    }
    fn json_schema(_: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        use schemars::schema::{InstanceType, Metadata, SchemaObject};
        SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            format: Some("date-time".into()),
            metadata: Some(Box::new(Metadata {
                description: Some("RFC3339 UTC timestamp, e.g. 2026-09-05T00:21:00Z".into()),
                ..Default::default()
            })),
            ..Default::default()
        }
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything on the wire is RFC3339 UTC (§2). API, Web and the host all
    /// parse these strings, so the exact rendering is a contract.
    #[test]
    fn a_timestamp_renders_as_rfc3339_utc_with_a_z() {
        let t = Timestamp::parse_rfc3339("2026-09-05T00:21:00Z").unwrap();
        assert_eq!(t.to_rfc3339(), "2026-09-05T00:21:00Z");
        assert_eq!(t.to_string(), "2026-09-05T00:21:00Z");
        assert_eq!(
            serde_json::to_string(&t).unwrap(),
            "\"2026-09-05T00:21:00Z\""
        );
    }

    /// An offset timestamp is accepted but NORMALISED to UTC: two clients in
    /// different zones must not produce two spellings of one instant.
    #[test]
    fn an_offset_is_converted_to_utc_rather_than_preserved() {
        let t = Timestamp::parse_rfc3339("2026-09-05T02:21:00+02:00").unwrap();
        assert_eq!(t.to_rfc3339(), "2026-09-05T00:21:00Z");
        assert_eq!(t, Timestamp::parse_rfc3339("2026-09-05T00:21:00Z").unwrap());
    }

    #[test]
    fn a_timestamp_round_trips_through_json() {
        let t = Timestamp::now();
        let json = serde_json::to_string(&t).unwrap();
        let back: Timestamp = serde_json::from_str(&json).unwrap();
        assert_eq!(back.to_rfc3339(), t.to_rfc3339());
    }

    #[test]
    fn a_string_that_is_not_a_timestamp_is_refused_on_the_wire() {
        for bad in [
            "\"\"",
            "\"not a date\"",
            "\"2026-09-05\"",
            "\"2026-09-05 00:21:00\"",
            "\"2026-13-05T00:21:00Z\"",
            "1757030460",
        ] {
            assert!(
                serde_json::from_str::<Timestamp>(bad).is_err(),
                "{bad} must not deserialize into a Timestamp"
            );
        }
    }

    /// Message ordering and log cursors compare timestamps, so `Ord` has to
    /// agree with chronology across offsets, not with the string.
    #[test]
    fn timestamps_order_chronologically_across_offsets() {
        let earlier = Timestamp::parse_rfc3339("2026-09-05T00:21:00Z").unwrap();
        let later = Timestamp::parse_rfc3339("2026-09-05T00:22:00Z").unwrap();
        assert!(earlier < later);
        // Same instant, two spellings: equal, not merely close.
        let same = Timestamp::parse_rfc3339("2026-09-05T02:21:00+02:00").unwrap();
        assert_eq!(earlier, same);
        assert!(!(earlier < same) && !(same < earlier));
    }

    #[test]
    fn now_is_utc_and_round_trips_through_the_inner_type() {
        let t = Timestamp::now();
        assert!(t.to_rfc3339().ends_with('Z'));
        let inner = t.into_inner();
        assert_eq!(Timestamp::from(inner), t);
        // Sub-second precision survives the string form.
        assert_eq!(
            Timestamp::parse_rfc3339(&t.to_rfc3339())
                .unwrap()
                .to_rfc3339(),
            t.to_rfc3339()
        );
    }
}
