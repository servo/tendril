// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use serde::{
    de::{Error, Visitor},
    Deserialize, Serialize, Serializer,
};

use crate::StrTendril;
use std::fmt;

impl Serialize for StrTendril {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self[..].serialize(serializer)
    }
}

struct TendrilVisitor;

impl<'de> Visitor<'de> for TendrilVisitor {
    type Value = StrTendril;

    fn visit_str<E: Error>(self, v: &str) -> Result<Self::Value, E> {
        Ok(StrTendril::from_slice(v))
    }

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "a tendril string")
    }
}

impl<'de> Deserialize<'de> for StrTendril {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_str(TendrilVisitor)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_serialize_deserialize() {
        let original = "test string";
        let original_tendril = StrTendril::from_slice(original);
        let encoded = serde_json::to_string(&original_tendril).unwrap();
        assert_eq!(encoded, r#""test string""#);
        let decoded_tendril: StrTendril = serde_json::from_str(&encoded).unwrap();
        assert_eq!(original_tendril, decoded_tendril);
        assert_eq!(&decoded_tendril[..], original);
    }
}
