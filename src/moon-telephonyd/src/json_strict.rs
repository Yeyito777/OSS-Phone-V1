//! Rejects JSON objects with duplicate keys at any depth. serde_json keeps
//! the last value silently, which would let `{"id":7,...,"id":"n42"}` lose a
//! field without an error (docs/telephony-protocol.md §1).

use std::collections::HashSet;
use std::fmt;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

struct NoDuplicates;

impl<'de> Deserialize<'de> for NoDuplicates {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(Check)
    }
}

struct Check;

impl<'de> Visitor<'de> for Check {
    type Value = NoDuplicates;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("JSON without duplicate object keys")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<NoDuplicates, A::Error> {
        let mut seen = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(de::Error::custom(format!("duplicate key {key:?}")));
            }
            map.next_value::<NoDuplicates>()?;
        }
        Ok(NoDuplicates)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<NoDuplicates, A::Error> {
        while seq.next_element::<NoDuplicates>()?.is_some() {}
        Ok(NoDuplicates)
    }

    fn visit_bool<E>(self, _: bool) -> Result<NoDuplicates, E> {
        Ok(NoDuplicates)
    }
    fn visit_i64<E>(self, _: i64) -> Result<NoDuplicates, E> {
        Ok(NoDuplicates)
    }
    fn visit_u64<E>(self, _: u64) -> Result<NoDuplicates, E> {
        Ok(NoDuplicates)
    }
    fn visit_f64<E>(self, _: f64) -> Result<NoDuplicates, E> {
        Ok(NoDuplicates)
    }
    fn visit_str<E>(self, _: &str) -> Result<NoDuplicates, E> {
        Ok(NoDuplicates)
    }
    fn visit_unit<E>(self) -> Result<NoDuplicates, E> {
        Ok(NoDuplicates)
    }
}

/// Err with a description when `bytes` is not JSON or repeats a key.
pub fn check(bytes: &[u8]) -> Result<(), String> {
    serde_json::from_slice::<NoDuplicates>(bytes)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::check;

    #[test]
    fn duplicates_are_found_at_any_depth() {
        assert!(check(br#"{"id":7,"cmd":"notify.dismiss","notification":"n42"}"#).is_ok());
        assert!(
            check(br#"{"id":7,"cmd":"notify.dismiss","id":"n42"}"#)
                .unwrap_err()
                .contains("duplicate key \"id\"")
        );
        assert!(check(br#"{"a":[{"x":1,"x":2}]}"#).is_err());
        assert!(check(br#"{"a":{"b":{"c":1}},"b":[1,"x",null,true,2.5]}"#).is_ok());
        assert!(check(b"{not json").is_err());
    }
}
