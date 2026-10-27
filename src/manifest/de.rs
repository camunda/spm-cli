//! Serde helpers that make the model reject what the schema rejects: explicit
//! `null`s, non-object specs and duplicate dependency names.

use super::{DependencyKind, SkillSpec};
use crate::ids::DepName;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Deserialize an optional field that, when present, must hold a value. Serde
/// reads an explicit `null` into an `Option` as absent, a value the schema
/// rejects; with `#[serde(default)]` a missing field is still `None`.
pub(super) fn present<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(d).map(Some)
}

/// Deserialize a `T` from a JSON object only. A derived struct impl also
/// accepts a positional array (`["url", "v1"]`), a shape the schema does not
/// have.
pub(super) struct ObjectOnly<T>(pub(super) T);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for ObjectOnly<T> {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{value::MapAccessDeserializer, MapAccess, Visitor};
        struct ObjectVisitor<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for ObjectVisitor<T> {
            type Value = T;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<M>(self, map: M) -> Result<T, M::Error>
            where
                M: MapAccess<'de>,
            {
                T::deserialize(MapAccessDeserializer::new(map))
            }
        }
        d.deserialize_map(ObjectVisitor(std::marker::PhantomData))
            .map(ObjectOnly)
    }
}

/// Deserialize the `skills` map while rejecting duplicate names. A plain
/// `BTreeMap` (or `serde_json::Value`) silently collapses repeated JSON object
/// keys to last-wins, which would drop an earlier skill without warning. We see
/// both entries only by streaming the map ourselves — so `Manifest::load` must
/// deserialize from the raw text, not from an already-collapsed `Value`.
pub(super) fn deserialize_unique_skills<'de, D>(
    d: D,
) -> Result<BTreeMap<DepName, SkillSpec>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_unique_map(d, DependencyKind::Skill)
}

/// Same duplicate-key rejection as [`deserialize_unique_skills`], for the
/// `plugins` map.
pub(super) fn deserialize_unique_plugins<'de, D>(
    d: D,
) -> Result<BTreeMap<DepName, SkillSpec>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_unique_map(d, DependencyKind::Plugin)
}

/// Stream a `{name: SkillSpec}` map, rejecting duplicate `name` keys. `kind`
/// only shapes the error message. Shared by the `skills` and `plugins` maps so
/// both get identical last-wins-collapse protection.
fn deserialize_unique_map<'de, D>(
    d: D,
    kind: DependencyKind,
) -> Result<BTreeMap<DepName, SkillSpec>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, MapAccess, Visitor};
    struct MapVisitor(DependencyKind);
    impl<'de> Visitor<'de> for MapVisitor {
        type Value = BTreeMap<DepName, SkillSpec>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "a map of unique {} names", self.0)
        }
        fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
        where
            M: MapAccess<'de>,
        {
            let kind = self.0;
            let mut out = BTreeMap::new();
            while let Some(name) = map.next_key::<DepName>()? {
                // Name the entry: serde's own message ("unknown field `x`",
                // "invalid type: null") does not say which one is wrong.
                let ObjectOnly(spec) = map
                    .next_value::<ObjectOnly<SkillSpec>>()
                    .map_err(|e| de::Error::custom(format!("{kind} `{name}`: {e}")))?;
                if out.contains_key(&name) {
                    return Err(de::Error::custom(format!(
                        "duplicate {kind} name `{name}` in the `{kind}s` map; \
                         each {kind} name may appear only once — remove or rename \
                         one of the `{name}` entries"
                    )));
                }
                out.insert(name, spec);
            }
            Ok(out)
        }
    }
    d.deserialize_map(MapVisitor(kind))
}
