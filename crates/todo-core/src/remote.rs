//! A tracker issue as the sync engine hands it over (design 5.2), and what
//! applying it does to an Item (5.4 steps 3 and 4): pure, so the store only
//! records the changes. Core fields are the tracker's (3.2); Overlay fields
//! are never touched.

use std::collections::BTreeMap;

use jiff::civil::Date;
use jiff::{SignedDuration, Timestamp};

use crate::field::ItemField;
use crate::ids::ItemId;
use crate::item::{CreatedVia, Item, LinkState, PersonRef, Priority, RemoteLink};
use crate::mutation::FieldChange;
use crate::status::{ResolutionKind, StatusCategory, TodoRefinement};
use crate::value::FieldValue;

/// A tracker person, until People (design 3.5): who, in their words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteUser {
    /// The tracker's stable id (a Data Center user key).
    pub id: String,
    pub display_name: String,
}

/// A normalised tracker issue (design 5.2): Core fields in local vocabulary.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteIssue {
    pub tracker_kind: String,
    pub instance_id: String,
    /// The join key; never changes.
    pub remote_id: String,
    /// Display only (moves change it, 5.5).
    pub key: String,
    pub url: String,
    pub title: String,
    /// Markdown: the adapter converts the tracker's text (wiki markup on
    /// Data Center), keeping what does not convert (ADR-0009).
    pub description: String,
    pub item_type: String,
    pub status_category: StatusCategory,
    /// Set in the `Done` category: done or cancelled, by resolution (5.8).
    pub resolution_kind: Option<ResolutionKind>,
    pub remote_status_name: String,
    pub resolution: Option<String>,
    pub priority: Priority,
    pub assignee: Option<RemoteUser>,
    pub reporter: Option<RemoteUser>,
    pub labels: Vec<String>,
    pub components: Vec<String>,
    pub fix_versions: Vec<String>,
    pub due_date: Option<Date>,
    /// The parent issue's `remote_id` (a subtask).
    pub parent_remote_id: Option<String>,
    pub sprint: Option<String>,
    pub story_points: Option<f64>,
    pub original_estimate: Option<SignedDuration>,
    pub remaining_estimate: Option<SignedDuration>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    /// Tracker fields with no local home, raw.
    pub extras: BTreeMap<String, serde_json::Value>,
    /// Assigned to the authenticated user (5.9).
    pub is_mine: bool,
}

/// A person as Core stores one until People exist: their display name.
fn person(u: &Option<RemoteUser>) -> Option<PersonRef> {
    u.as_ref().map(|u| {
        PersonRef::FreeText(if u.display_name.is_empty() {
            u.id.clone()
        } else {
            u.display_name.clone()
        })
    })
}

/// A new Item for `issue` (`created_via = remote_import`): its status from
/// the category, a To Do issue landing in Next (5.4, open question 5).
pub fn import(
    id: ItemId,
    issue: &RemoteIssue,
    parent: Option<ItemId>,
    scopes: &[String],
    now: Timestamp,
) -> Item {
    let mut item = Item::new_capture(id, issue.title.clone(), CreatedVia::RemoteImport, now);
    item.overlay.todo_refinement = TodoRefinement::Next;
    write(&mut item, issue, parent);
    item.remote_link = Some(link(issue, LinkState::Ok, false));
    item.derived.scopes = scopes.to_vec();
    item.derived.is_mine = issue.is_mine;
    item.derived.staleness.last_remote_change_at = Some(issue.updated_at);
    item.derived.staleness.status_entered_at = Some(now);
    item
}

fn link(issue: &RemoteIssue, state: LinkState, pinned: bool) -> RemoteLink {
    RemoteLink {
        tracker_kind: issue.tracker_kind.clone(),
        instance_id: issue.instance_id.clone(),
        remote_id: issue.remote_id.clone(),
        key: issue.key.clone(),
        url: issue.url.clone(),
        last_pulled_version: Some(issue.updated_at),
        link_state: state,
        pinned,
    }
}

/// In `remote_extras`: the parent issue's remote id while that issue has no
/// Item here yet (pages arrive in any order), so a later page adopts it.
pub const PENDING_PARENT: &str = "todo-tui.parent_remote_id";

/// Writes `issue`'s Core fields into `item`.
fn write(item: &mut Item, issue: &RemoteIssue, parent: Option<ItemId>) {
    let c = &mut item.core;
    c.title = if issue.title.trim().is_empty() {
        issue.key.clone()
    } else {
        issue.title.clone()
    };
    c.description = issue.description.clone();
    c.item_type = issue.item_type.clone();
    c.status_category = issue.status_category;
    c.resolution_kind = (issue.status_category == StatusCategory::Done)
        .then_some(issue.resolution_kind.unwrap_or(ResolutionKind::Done));
    c.remote_status_name = Some(issue.remote_status_name.clone()).filter(|s| !s.is_empty());
    c.resolution = issue.resolution.clone();
    c.priority = issue.priority;
    c.assignee = person(&issue.assignee);
    c.reporter = person(&issue.reporter);
    c.labels = issue.labels.clone();
    c.components = issue.components.clone();
    c.fix_versions = issue.fix_versions.clone();
    c.due_date = issue.due_date;
    c.parent_id = parent;
    c.sprint = issue.sprint.clone();
    c.story_points = issue.story_points;
    c.original_estimate = issue.original_estimate;
    c.remaining_estimate = issue.remaining_estimate;
    c.remote_created_at = Some(issue.created_at);
    c.remote_updated_at = Some(issue.updated_at);
    item.remote_extras = issue.extras.clone();
    if let (None, Some(p)) = (parent, &issue.parent_remote_id) {
        item.remote_extras.insert(
            PENDING_PARENT.to_owned(),
            serde_json::Value::String(p.clone()),
        );
    }
}

/// `item`, a subtask whose parent issue had no Item, under `parent` now:
/// the change, or `None` when it was not waiting on it.
pub fn adopt(item: &Item, parent: ItemId, now: Timestamp) -> Option<(Item, Vec<FieldChange>)> {
    item.remote_extras.get(PENDING_PARENT)?;
    let mut next = item.clone();
    next.remote_extras.remove(PENDING_PARENT);
    next.core.parent_id = Some(parent);
    next.updated_at = now;
    let changes = [ItemField::ParentId, ItemField::RemoteExtras]
        .into_iter()
        .filter_map(|field| {
            let old = FieldValue::of(item, field);
            let new = FieldValue::of(&next, field);
            (old != new)
                .then(|| old.zip(new))
                .flatten()
                .map(|(old, new)| FieldChange { field, old, new })
        })
        .collect();
    Some((next, changes))
}

/// The Core fields a pull writes, in event order.
const SYNCED: [ItemField; 22] = [
    ItemField::Title,
    ItemField::Description,
    ItemField::ItemType,
    ItemField::StatusCategory,
    ItemField::ResolutionKind,
    ItemField::RemoteStatusName,
    ItemField::Resolution,
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
    ItemField::RemoteCreatedAt,
    ItemField::RemoteUpdatedAt,
    ItemField::RemoteLink,
];

/// `item` brought up to `issue` (5.4 step 4): every Core field that differs,
/// with a change each; the link's key and URL follow a move (5.5); Overlay
/// is left alone. Returns the Item and its changes, empty when nothing
/// differs (a page seen twice).
pub fn update(
    item: &Item,
    issue: &RemoteIssue,
    parent: Option<ItemId>,
    scopes: &[String],
    now: Timestamp,
) -> (Item, Vec<FieldChange>) {
    let mut next = item.clone();
    write(&mut next, issue, parent);
    let state = item
        .remote_link
        .as_ref()
        .map_or(LinkState::Ok, |l| l.link_state);
    let pinned = item.remote_link.as_ref().is_some_and(|l| l.pinned);
    // A link that was broken is whole again once the issue answers.
    let state = if state == LinkState::Broken {
        LinkState::Ok
    } else {
        state
    };
    let mut new_link = link(issue, state, pinned);
    // The pulled version alone is bookkeeping, not a change.
    if let Some(old) = &item.remote_link {
        let mut probe = new_link.clone();
        probe.last_pulled_version = old.last_pulled_version;
        if &probe == old {
            new_link = old.clone();
            new_link.last_pulled_version = Some(issue.updated_at);
        }
    }
    next.remote_link = Some(new_link);
    let mut changes = Vec::new();
    for field in SYNCED {
        let (Some(old), Some(new)) = (FieldValue::of(item, field), FieldValue::of(&next, field))
        else {
            continue;
        };
        let same = match (&old, &new) {
            (FieldValue::RemoteLink(a), FieldValue::RemoteLink(b)) => {
                let mut a = a.clone();
                a.last_pulled_version = b.last_pulled_version;
                &a == b
            }
            _ => old == new,
        };
        if !same {
            changes.push(FieldChange { field, old, new });
        }
    }
    next.derived.scopes = scopes.to_vec();
    next.derived.is_mine = issue.is_mine;
    if !changes.is_empty() {
        next.updated_at = now;
        next.derived.staleness.last_remote_change_at = Some(issue.updated_at);
        if next.status() != item.status() {
            next.derived.staleness.status_entered_at = Some(now);
        }
    }
    (next, changes)
}

/// A local `item` linked to `issue` (design 3.3, 5.7): by promote (the issue
/// was made from it) or attach (an existing one). It takes the issue's Core
/// fields as a pull would, keeps its Overlay, and is pinned, so it never
/// leaves the list for being outside every scope. Returns the Item, its
/// changes, and the pushed fields whose local value Jira's replaced: the
/// person's to have offered back (`link_conflict`, 3.3).
pub fn link_to(
    item: &Item,
    issue: &RemoteIssue,
    parent: Option<ItemId>,
    now: Timestamp,
) -> (Item, Vec<FieldChange>, Vec<FieldChange>) {
    let mut next = item.clone();
    write(&mut next, issue, parent.or(item.core.parent_id));
    next.remote_link = Some(link(issue, LinkState::Ok, true));
    next.derived.is_mine = issue.is_mine;
    next.updated_at = now;
    next.derived.staleness.last_remote_change_at = Some(issue.updated_at);
    let mut changes = Vec::new();
    let mut lost = Vec::new();
    for field in SYNCED {
        let (Some(old), Some(new)) = (FieldValue::of(item, field), FieldValue::of(&next, field))
        else {
            continue;
        };
        if old == new {
            continue;
        }
        // What the person had set and Jira did not take: worth offering back.
        // An empty local value lost nothing.
        let had = !matches!(&old, FieldValue::Cleared)
            && old != FieldValue::String(String::new())
            && old != FieldValue::Markdown(String::new())
            && old != FieldValue::StringSet(Vec::new())
            && old != FieldValue::Priority(Priority::None);
        if had && crate::mutation::pushed(field) && field != ItemField::ResolutionKind {
            lost.push(FieldChange {
                field,
                old: old.clone(),
                new: new.clone(),
            });
        }
        changes.push(FieldChange { field, old, new });
    }
    if next.status() != item.status() {
        next.derived.staleness.status_entered_at = Some(now);
    }
    (next, changes, lost)
}

/// The issue cannot be read any more (deleted, or permission lost, 5.5):
/// the link breaks; content stays.
pub fn break_link(item: &Item) -> Option<(Item, FieldChange)> {
    let link = item.remote_link.as_ref()?;
    if link.link_state == LinkState::Broken {
        return None;
    }
    let mut next = item.clone();
    let mut broken = link.clone();
    broken.link_state = LinkState::Broken;
    next.remote_link = Some(broken.clone());
    Some((
        next,
        FieldChange {
            field: ItemField::RemoteLink,
            old: FieldValue::RemoteLink(link.clone()),
            new: FieldValue::RemoteLink(broken),
        },
    ))
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;
    use crate::status::Status;

    fn ts(s: i64) -> Timestamp {
        Timestamp::from_second(s).unwrap()
    }

    pub(crate) fn issue() -> RemoteIssue {
        RemoteIssue {
            tracker_kind: "jira".into(),
            instance_id: "work-jira".into(),
            remote_id: "10001".into(),
            key: "PLAT-1".into(),
            url: "https://jira.example.com/browse/PLAT-1".into(),
            title: "Fix the login test".into(),
            description: "h2. Steps\n* one".into(),
            item_type: "Bug".into(),
            status_category: StatusCategory::Todo,
            resolution_kind: None,
            remote_status_name: "Open".into(),
            resolution: None,
            priority: Priority::High,
            assignee: Some(RemoteUser {
                id: "JIRAUSER1".into(),
                display_name: "Dana Example".into(),
            }),
            reporter: None,
            labels: vec!["auth".into()],
            components: vec!["Web".into()],
            fix_versions: vec![],
            due_date: Some(date(2026, 11, 6)),
            parent_remote_id: None,
            sprint: None,
            story_points: Some(3.0),
            original_estimate: None,
            remaining_estimate: None,
            created_at: ts(100),
            updated_at: ts(200),
            extras: BTreeMap::new(),
            is_mine: true,
        }
    }

    #[test]
    fn an_import_lands_in_next_with_its_link() {
        let item = import(crate::new_ulid(), &issue(), None, &["mine".into()], ts(300));
        assert_eq!(item.status(), Status::Next);
        assert_eq!(item.created_via, CreatedVia::RemoteImport);
        let link = item.remote_link.as_ref().unwrap();
        assert_eq!(
            (link.key.as_str(), link.link_state, link.pinned),
            ("PLAT-1", LinkState::Ok, false)
        );
        assert_eq!(
            item.core.assignee,
            Some(PersonRef::FreeText("Dana Example".into()))
        );
        assert_eq!(item.derived.scopes, ["mine"]);
        assert!(item.derived.is_mine);
        // Done and cancelled by resolution.
        let mut done = issue();
        done.status_category = StatusCategory::Done;
        assert_eq!(
            import(crate::new_ulid(), &done, None, &[], ts(1)).status(),
            Status::Done
        );
        done.resolution_kind = Some(ResolutionKind::Cancelled);
        assert_eq!(
            import(crate::new_ulid(), &done, None, &[], ts(1)).status(),
            Status::Cancelled
        );
        let mut started = issue();
        started.status_category = StatusCategory::InProgress;
        assert_eq!(
            import(crate::new_ulid(), &started, None, &[], ts(1)).status(),
            Status::InProgress
        );
    }

    #[test]
    fn an_update_changes_core_only_and_nothing_twice() {
        let mut item = import(crate::new_ulid(), &issue(), None, &["mine".into()], ts(300));
        item.overlay.private_notes = "mine".into();
        item.overlay.local_priority_override = Some(Priority::Low);
        let (same, none) = update(&item, &issue(), None, &["mine".into()], ts(400));
        assert!(none.is_empty(), "{none:?}");
        assert_eq!(same.core, item.core);
        let mut edited = issue();
        edited.title = "Fix the flaky login test".into();
        edited.priority = Priority::Highest;
        edited.key = "WEB-9".into();
        edited.url = "https://jira.example.com/browse/WEB-9".into();
        edited.status_category = StatusCategory::InProgress;
        edited.updated_at = ts(500);
        let (next, changes) = update(&item, &edited, None, &["mine".into()], ts(600));
        let fields: Vec<_> = changes.iter().map(|c| c.field).collect();
        assert_eq!(
            fields,
            [
                ItemField::Title,
                ItemField::StatusCategory,
                ItemField::Priority,
                ItemField::RemoteUpdatedAt,
                ItemField::RemoteLink,
            ]
        );
        assert_eq!(next.status(), Status::InProgress);
        assert_eq!(next.remote_link.as_ref().unwrap().key, "WEB-9");
        // Overlay untouched.
        assert_eq!(next.overlay.private_notes, "mine");
        assert_eq!(next.overlay.local_priority_override, Some(Priority::Low));
        assert_eq!(next.updated_at, ts(600));
        // A later pull of an unchanged issue only moves the pulled version.
        let mut later = edited.clone();
        later.updated_at = ts(500);
        let (_, again) = update(&next, &later, None, &["mine".into()], ts(700));
        assert!(again.is_empty(), "{again:?}");
    }

    #[test]
    fn a_broken_link_breaks_once_and_heals_on_the_next_pull() {
        let item = import(crate::new_ulid(), &issue(), None, &[], ts(300));
        let (broken, change) = break_link(&item).unwrap();
        assert_eq!(change.field, ItemField::RemoteLink);
        assert_eq!(
            broken.remote_link.as_ref().unwrap().link_state,
            LinkState::Broken
        );
        assert!(break_link(&broken).is_none());
        let (healed, changes) = update(&broken, &issue(), None, &[], ts(400));
        assert_eq!(
            healed.remote_link.as_ref().unwrap().link_state,
            LinkState::Ok
        );
        assert_eq!(
            changes.iter().map(|c| c.field).collect::<Vec<_>>(),
            [ItemField::RemoteLink]
        );
    }
}
