//! JSON field presence is independent of whether its supplied value is null.
use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A supplied value that may explicitly be null. Unlike Option, this type does
/// not deserialize a missing object member as null.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Nullable<T> {
    Null,
    Value(T),
}

/// Missing and supplied values remain distinct. `model_schema` supplies the
/// enclosing field's serde default and omission attributes automatically.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Presence<T> {
    #[default]
    Missing,
    Present(T),
}

impl<T> Presence<T> {
    pub fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Presence<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Self::Present)
    }
}

impl<T: Serialize> Serialize for Presence<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Present(value) => value.serialize(serializer),
            Self::Missing => Err(serde::ser::Error::custom(
                "missing fields must be omitted by the enclosing object",
            )),
        }
    }
}

impl<T: JsonSchema> JsonSchema for Nullable<T> {
    fn schema_name() -> Cow<'static, str> {
        format!("Nullable_{}", T::schema_name()).into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        <Option<T>>::json_schema(generator)
    }
}

impl<T: JsonSchema> JsonSchema for Presence<T> {
    fn schema_name() -> Cow<'static, str> {
        format!("Presence_{}", T::schema_name()).into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        T::json_schema(generator)
    }

    fn _schemars_private_is_option() -> bool {
        true
    }

    fn _schemars_private_non_optional_json_schema(generator: &mut SchemaGenerator) -> Schema {
        T::json_schema(generator)
    }
}
