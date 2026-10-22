//! Decoding a signed CBOR map whose unknown keys must survive under the signature.
//!
//! [Versioning — invariant 2] requires every signed record to keep the keys its reader does not
//! know, re-emit them canonically, and cover them with the signature. A record does that by
//! carrying a `#[serde(flatten)] unknown: BTreeMap<String, Value>` (wire-absent when empty, so no
//! existing signature moves) and a hand-written `Deserialize` built on [`OpenMap`]: it takes each
//! known field out by name and hands back whatever is left as the unknown map.
//!
//! The decoder is hand-written rather than `serde(flatten)` on the way in because flatten
//! buffers unknown values through serde's private content type, which cannot hold a CBOR tag or
//! an integer wider than 64 bits — a future field carrying either would make the whole record
//! undecodable instead of preserved.
//!
//! [Versioning — invariant 2]: https://docs/design/versioning/#system-invariants

use std::collections::BTreeMap;

use ciborium::value::Value;
use serde::Deserialize;
use serde::de::{DeserializeOwned, Error};

/// A decoded text-keyed CBOR map, drained field by field into a signed record.
#[derive(Debug)]
pub struct OpenMap {
    record: &'static str,
    fields: BTreeMap<String, Value>,
}

impl OpenMap {
    /// Decode `d` as a map with only text keys and no duplicate key. `record` names the record
    /// in every error and log line.
    ///
    /// # Errors
    ///
    /// When the input is not a map, holds a non-text key, or repeats a key.
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        record: &'static str,
        d: D,
    ) -> Result<Self, D::Error> {
        let Value::Map(entries) = Value::deserialize(d)? else {
            return Err(D::Error::custom(format!("{record} must be a map")));
        };
        let mut fields = BTreeMap::new();
        for (key, value) in entries {
            let Value::Text(key) = key else {
                return Err(D::Error::custom(format!("{record} has only text keys")));
            };
            if fields.contains_key(&key) {
                return Err(D::Error::custom(format!("duplicate {record} field {key}")));
            }
            fields.insert(key, value);
        }
        Ok(Self { record, fields })
    }

    /// Take a required field.
    ///
    /// # Errors
    ///
    /// When the key is absent or its value does not decode as `T`.
    pub fn take<T: DeserializeOwned, E: Error>(&mut self, key: &'static str) -> Result<T, E> {
        let value = self
            .fields
            .remove(key)
            .ok_or_else(|| E::missing_field(key))?;
        self.decode(key, value)
    }

    /// Take an optional field: an absent key and a present `null` both read as `None`.
    ///
    /// # Errors
    ///
    /// When a present, non-null value does not decode as `T`.
    pub fn take_opt<T: DeserializeOwned, E: Error>(
        &mut self,
        key: &'static str,
    ) -> Result<Option<T>, E> {
        match self.fields.remove(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => self.decode(key, value).map(Some),
        }
    }

    /// The keys no `take` claimed: the record's unknown map, preserved verbatim.
    #[must_use]
    pub fn into_unknown(self) -> BTreeMap<String, Value> {
        if !self.fields.is_empty() {
            tracing::debug!(
                record = self.record,
                unknown_keys = ?self.fields.keys().collect::<Vec<_>>(),
                "cbor: preserving keys this build does not know"
            );
        }
        self.fields
    }

    fn decode<T: DeserializeOwned, E: Error>(&self, key: &str, value: Value) -> Result<T, E> {
        value
            .deserialized()
            .map_err(|e| E::custom(format!("{} field {key}: {e}", self.record)))
    }
}

#[cfg(test)]
mod tests;
