//! Relations between Items (design 3.6).

use serde::{Deserialize, Serialize};

use crate::ids::ItemId;

/// What a relation says about its ends. Persisted in events as snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    /// `from` blocks `to`.
    Blocks,
    /// `from` duplicates `to`.
    Duplicates,
    /// Symmetric; stored canonically, see [`Relation::new`].
    RelatesTo,
}

/// A relation between two distinct Items. The whole triple is its identity.
///
/// Built only through [`Relation::new`], which also guards deserialization,
/// so every value is canonical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "RelationRepr")]
pub struct Relation {
    from: ItemId,
    to: ItemId,
    kind: RelationKind,
}

/// Why a relation cannot be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RelationError {
    #[error("an Item cannot be related to itself")]
    SameItem,
}

impl Relation {
    /// Rejects `from == to`. A `relates_to` relation is symmetric, so its ends
    /// are swapped when needed to give `from < to`. The order is the ids'
    /// natural order, which for ULIDs is also the order of their canonical
    /// 26-character strings, so SQL can compare the TEXT columns.
    pub fn new(from: ItemId, to: ItemId, kind: RelationKind) -> Result<Self, RelationError> {
        if from == to {
            return Err(RelationError::SameItem);
        }
        let (from, to) = match kind {
            RelationKind::RelatesTo if to < from => (to, from),
            _ => (from, to),
        };
        Ok(Self { from, to, kind })
    }

    pub fn from(&self) -> ItemId {
        self.from
    }

    pub fn to(&self) -> ItemId {
        self.to
    }

    pub fn kind(&self) -> RelationKind {
        self.kind
    }
}

/// The serde form of [`Relation`], checked through [`Relation::new`].
#[derive(Deserialize)]
struct RelationRepr {
    from: ItemId,
    to: ItemId,
    kind: RelationKind,
}

impl TryFrom<RelationRepr> for Relation {
    type Error = RelationError;
    fn try_from(r: RelationRepr) -> Result<Self, RelationError> {
        Relation::new(r.from, r.to, r.kind)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use ulid::Ulid;

    use super::*;

    fn id(n: u128) -> Ulid {
        Ulid::from(n)
    }

    #[test]
    fn new_rejects_self_relations() {
        for kind in [
            RelationKind::Blocks,
            RelationKind::Duplicates,
            RelationKind::RelatesTo,
        ] {
            assert_eq!(
                Relation::new(id(1), id(1), kind),
                Err(RelationError::SameItem)
            );
        }
    }

    #[test]
    fn only_relates_to_is_canonicalised() {
        let rows = [
            (RelationKind::Blocks, (2, 1), (2, 1)),
            (RelationKind::Duplicates, (2, 1), (2, 1)),
            (RelationKind::RelatesTo, (2, 1), (1, 2)),
            (RelationKind::RelatesTo, (1, 2), (1, 2)),
        ];
        for (kind, (from, to), want) in rows {
            let r = Relation::new(id(from), id(to), kind).unwrap();
            assert_eq!((r.from(), r.to(), r.kind()), (id(want.0), id(want.1), kind));
        }
        assert_eq!(
            Relation::new(id(1), id(2), RelationKind::RelatesTo),
            Relation::new(id(2), id(1), RelationKind::RelatesTo)
        );
        assert_ne!(
            Relation::new(id(1), id(2), RelationKind::Blocks),
            Relation::new(id(2), id(1), RelationKind::Blocks)
        );
    }

    /// The canonical order holds for the string form the store keeps.
    #[test]
    fn id_order_matches_string_order() {
        let a: Ulid = "01JB8X4Z5Q6R7S8T9V0W1X2Y3Z".parse().unwrap();
        let b: Ulid = "01JB8X4Z5Q6R7S8T9V0W1X2Y40".parse().unwrap();
        let c = Ulid::from(u128::MAX >> 2);
        for (lo, hi) in [(a, b), (b, c), (id(9), id(10)), (id(255), id(256))] {
            assert!(lo < hi);
            assert!(lo.to_string() < hi.to_string(), "{lo} {hi}");
        }
        let r = Relation::new(b, a, RelationKind::RelatesTo).unwrap();
        assert!(r.from().to_string() < r.to().to_string());
    }

    #[test]
    fn golden_json() {
        let a: Ulid = "01JB8X4Z5Q6R7S8T9V0W1X2Y3Z".parse().unwrap();
        let b: Ulid = "01JB8X4Z5Q6R7S8T9V0W1X2Y40".parse().unwrap();
        let rows = [
            (
                Relation::new(b, a, RelationKind::Blocks).unwrap(),
                json!({"from": "01JB8X4Z5Q6R7S8T9V0W1X2Y40", "to": "01JB8X4Z5Q6R7S8T9V0W1X2Y3Z", "kind": "blocks"}),
            ),
            (
                Relation::new(a, b, RelationKind::Duplicates).unwrap(),
                json!({"from": "01JB8X4Z5Q6R7S8T9V0W1X2Y3Z", "to": "01JB8X4Z5Q6R7S8T9V0W1X2Y40", "kind": "duplicates"}),
            ),
            (
                Relation::new(b, a, RelationKind::RelatesTo).unwrap(),
                json!({"from": "01JB8X4Z5Q6R7S8T9V0W1X2Y3Z", "to": "01JB8X4Z5Q6R7S8T9V0W1X2Y40", "kind": "relates_to"}),
            ),
        ];
        for (relation, want) in rows {
            assert_eq!(serde_json::to_value(relation).unwrap(), want);
            assert_eq!(serde_json::from_value::<Relation>(want).unwrap(), relation);
        }
    }

    #[test]
    fn deserialization_goes_through_new() {
        let a = "01JB8X4Z5Q6R7S8T9V0W1X2Y3Z";
        let b = "01JB8X4Z5Q6R7S8T9V0W1X2Y40";
        let swapped: Relation =
            serde_json::from_value(json!({"from": b, "to": a, "kind": "relates_to"})).unwrap();
        assert_eq!(swapped.from().to_string(), a);
        for bad in [
            json!({"from": a, "to": a, "kind": "blocks"}),
            json!({"from": a, "to": b, "kind": "parent"}),
            json!({"from": a, "kind": "blocks"}),
        ] {
            assert!(
                serde_json::from_value::<Relation>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn sorts_by_from_to_kind() {
        let mut v = [
            Relation::new(id(2), id(1), RelationKind::Blocks).unwrap(),
            Relation::new(id(1), id(3), RelationKind::RelatesTo).unwrap(),
            Relation::new(id(1), id(3), RelationKind::Blocks).unwrap(),
        ];
        v.sort();
        let keys: Vec<_> = v.iter().map(|r| (r.from(), r.to(), r.kind())).collect();
        assert_eq!(
            keys,
            [
                (id(1), id(3), RelationKind::Blocks),
                (id(1), id(3), RelationKind::RelatesTo),
                (id(2), id(1), RelationKind::Blocks),
            ]
        );
    }
}
