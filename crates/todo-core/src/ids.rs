use ulid::Ulid;

/// Local Item id. Assigned at creation; never changes, never reused.
pub type ItemId = Ulid;
/// Local Person id.
pub type PersonId = Ulid;
/// The store's global, monotonically increasing revision (design 4.4).
pub type Revision = u64;

/// A fresh ULID for ids, op ids and request ids.
pub fn new_ulid() -> Ulid {
    Ulid::generate()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulids_are_unique_and_roundtrip_as_strings() {
        let a = new_ulid();
        let b = new_ulid();
        assert_ne!(a, b);
        assert_eq!(a.to_string().parse::<Ulid>().unwrap(), a);
        assert_eq!(a.to_string().len(), 26);
    }
}
