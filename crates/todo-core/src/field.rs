use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Addressable Item fields. The discriminant is the `todo.v1.ItemField`
/// number: the Item field number for top-level fields (below 1000), and
/// `parent * 1000 + sub` for a field inside an Item sub-message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u32)]
pub enum ItemField {
    Id = 1,
    CreatedVia = 2,
    CreatedAt = 3,
    UpdatedAt = 4,
    Revision = 5,
    RemoteLink = 6,
    Title = 10,
    Description = 11,
    ItemType = 12,
    StatusCategory = 13,
    ResolutionKind = 14,
    RemoteStatusName = 15,
    Priority = 16,
    Assignee = 17,
    Reporter = 18,
    Labels = 19,
    Components = 20,
    FixVersions = 21,
    DueDate = 22,
    ParentId = 23,
    Sprint = 24,
    StoryPoints = 25,
    OriginalEstimate = 26,
    RemainingEstimate = 27,
    Resolution = 28,
    RemoteCreatedAt = 29,
    RemoteUpdatedAt = 30,
    TodoRefinement = 40,
    Waiting = 41,
    DeferUntil = 42,
    PrivateNotes = 43,
    FocusWeek = 44,
    LocalPriorityOverride = 45,
    Archived = 46,
    Recurrence = 47,
    RemoteExtras = 60,
    Status = 70,
    EffectivePriority = 71,
    IsMine = 72,
    PendingFields = 73,
    HasConflict = 74,
    HasPushFailure = 75,
    DescriptionLossy = 76,
    DescriptionDirty = 77,
    Staleness = 78,
    Scopes = 79,
    DeletedAt = 80,
    MergedInto = 81,
    IsOverdue = 82,
    WaitingOn = 41001,
    WaitingReason = 41002,
    WaitingSince = 41003,
    WaitingFollowUpOn = 41004,
    ArchivedAt = 46001,
    ArchivedReason = 46002,
    RemoteLinkTrackerKind = 6001,
    RemoteLinkInstanceId = 6002,
    RemoteLinkRemoteId = 6003,
    RemoteLinkKey = 6004,
    RemoteLinkUrl = 6005,
    RemoteLinkLastPulledVersion = 6006,
    RemoteLinkLinkState = 6007,
    RemoteLinkPinned = 6008,
    RecurrenceSeriesId = 47001,
    RecurrenceOccurrenceDate = 47002,
    StalenessLastUserTouchAt = 78001,
    StalenessLastRemoteChangeAt = 78002,
    StalenessStatusEnteredAt = 78003,
    StalenessWaitingSince = 78004,
}

impl ItemField {
    pub const ALL: &[ItemField] = &[
        ItemField::Id,
        ItemField::CreatedVia,
        ItemField::CreatedAt,
        ItemField::UpdatedAt,
        ItemField::Revision,
        ItemField::RemoteLink,
        ItemField::Title,
        ItemField::Description,
        ItemField::ItemType,
        ItemField::StatusCategory,
        ItemField::ResolutionKind,
        ItemField::RemoteStatusName,
        ItemField::Priority,
        ItemField::Assignee,
        ItemField::Reporter,
        ItemField::Labels,
        ItemField::Components,
        ItemField::FixVersions,
        ItemField::DueDate,
        ItemField::ParentId,
        ItemField::Sprint,
        ItemField::StoryPoints,
        ItemField::OriginalEstimate,
        ItemField::RemainingEstimate,
        ItemField::Resolution,
        ItemField::RemoteCreatedAt,
        ItemField::RemoteUpdatedAt,
        ItemField::TodoRefinement,
        ItemField::Waiting,
        ItemField::DeferUntil,
        ItemField::PrivateNotes,
        ItemField::FocusWeek,
        ItemField::LocalPriorityOverride,
        ItemField::Archived,
        ItemField::Recurrence,
        ItemField::RemoteExtras,
        ItemField::Status,
        ItemField::EffectivePriority,
        ItemField::IsMine,
        ItemField::PendingFields,
        ItemField::HasConflict,
        ItemField::HasPushFailure,
        ItemField::DescriptionLossy,
        ItemField::DescriptionDirty,
        ItemField::Staleness,
        ItemField::Scopes,
        ItemField::DeletedAt,
        ItemField::MergedInto,
        ItemField::IsOverdue,
        ItemField::WaitingOn,
        ItemField::WaitingReason,
        ItemField::WaitingSince,
        ItemField::WaitingFollowUpOn,
        ItemField::ArchivedAt,
        ItemField::ArchivedReason,
        ItemField::RemoteLinkTrackerKind,
        ItemField::RemoteLinkInstanceId,
        ItemField::RemoteLinkRemoteId,
        ItemField::RemoteLinkKey,
        ItemField::RemoteLinkUrl,
        ItemField::RemoteLinkLastPulledVersion,
        ItemField::RemoteLinkLinkState,
        ItemField::RemoteLinkPinned,
        ItemField::RecurrenceSeriesId,
        ItemField::RecurrenceOccurrenceDate,
        ItemField::StalenessLastUserTouchAt,
        ItemField::StalenessLastRemoteChangeAt,
        ItemField::StalenessStatusEnteredAt,
        ItemField::StalenessWaitingSince,
    ];

    /// The `todo.v1.ItemField` number.
    pub fn number(self) -> u32 {
        self as u32
    }

    pub fn from_number(n: u32) -> Option<Self> {
        Self::ALL.iter().copied().find(|f| f.number() == n)
    }

    /// True for targets inside a nested message (`waiting.on`, ...).
    pub fn is_nested(self) -> bool {
        self.number() >= 1000
    }

    /// The top-level field holding a nested target (`waiting.on` -> `waiting`).
    pub fn parent(self) -> Option<ItemField> {
        self.is_nested()
            .then(|| ItemField::from_number(self.number() / 1000))
            .flatten()
    }
}

/// Serialized as the `todo.v1.ItemField` number, which never changes.
impl Serialize for ItemField {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(self.number())
    }
}

impl<'de> Deserialize<'de> for ItemField {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let n = u32::deserialize(deserializer)?;
        ItemField::from_number(n)
            .ok_or_else(|| D::Error::custom(format_args!("unknown ItemField number {n}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_round_trip_and_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for &f in ItemField::ALL {
            assert!(seen.insert(f.number()));
            assert_eq!(ItemField::from_number(f.number()), Some(f));
        }
        assert_eq!(ItemField::from_number(0), None);
        assert_eq!(ItemField::Title.number(), 10);
        assert_eq!(ItemField::WaitingOn.number(), 41001);
        for &f in ItemField::ALL {
            assert_eq!(f.is_nested(), f.parent().is_some());
        }
        assert_eq!(
            ItemField::RemoteLinkPinned.parent(),
            Some(ItemField::RemoteLink)
        );
    }

    #[test]
    fn serializes_as_its_number() {
        assert_eq!(
            serde_json::to_string(&ItemField::WaitingOn).unwrap(),
            "41001"
        );
        assert_eq!(
            serde_json::from_str::<ItemField>("16").unwrap(),
            ItemField::Priority
        );
        assert!(serde_json::from_str::<ItemField>("9").is_err());
        assert!(serde_json::from_str::<ItemField>("\"title\"").is_err());
    }
}
