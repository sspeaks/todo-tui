//! A Data Center issue's JSON into a normalised `RemoteIssue` (design 5.2):
//! Core fields in local vocabulary, the rest kept raw as extras.

use std::collections::BTreeMap;

use jiff::civil::Date;
use jiff::{SignedDuration, Timestamp};
use serde_json::Value;
use todo_core::remote::{RemoteIssue, RemoteUser};
use todo_core::{Priority, ResolutionKind, StatusCategory};

/// What discovery learnt about the instance (design 5.8).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Discovery {
    /// The authenticated user's key, for `is_mine` (5.9).
    pub me: String,
    /// The user's profile time zone, which JQL dates are read in.
    pub time_zone: String,
    /// Priority name → local level, by rank (5.8).
    pub priorities: BTreeMap<String, Priority>,
    /// Local level → the Jira name a push sets: `priority_map`'s, else the
    /// first of that rank in Jira's order (5.8).
    pub priority_names: BTreeMap<Priority, String>,
    /// Custom field ids, when the instance has them.
    pub story_points: Option<String>,
    pub sprint: Option<String>,
    /// Resolution names meaning cancelled; any other is done (5.8).
    pub cancelled_resolutions: Vec<String>,
}

/// The fields a search asks for: the Core ones, plus the custom fields.
pub fn search_fields(d: &Discovery) -> Vec<String> {
    let mut f: Vec<String> = [
        "summary",
        "description",
        "issuetype",
        "status",
        "priority",
        "assignee",
        "reporter",
        "labels",
        "components",
        "fixVersions",
        "duedate",
        "resolution",
        "created",
        "updated",
        "parent",
        "timeoriginalestimate",
        "timeestimate",
    ]
    .map(str::to_owned)
    .to_vec();
    f.extend(d.story_points.clone());
    f.extend(d.sprint.clone());
    // And every other navigable field, for the extras (design 5.2).
    f.push("*navigable".to_owned());
    f
}

/// Jira priorities in their order (highest first) onto the five local
/// levels (design 5.8): five map one to one, fewer spread across the
/// scale, more fold onto the nearest level.
pub fn map_priorities(names: &[String]) -> BTreeMap<String, Priority> {
    const LEVELS: [Priority; 5] = [
        Priority::Highest,
        Priority::High,
        Priority::Medium,
        Priority::Low,
        Priority::Lowest,
    ];
    let n = names.len();
    names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let level = if n <= 1 {
                2
            } else {
                // i / (n-1) of the way down the scale, rounded.
                ((i * 4 * 2 + (n - 1)) / (2 * (n - 1))).min(4)
            };
            (name.clone(), LEVELS[level])
        })
        .collect()
}

/// Both directions of the priority mapping (design 5.8), built together
/// so a level pushed reads back as itself: Jira name → level for pulls,
/// level → Jira name for pushes. Ranks first (`map_priorities`); then each
/// `priority_map` entry that names a Jira priority takes that level both
/// ways. A level with no name of its own pushes as the nearest one that
/// has (above on a tie).
pub fn priority_maps(
    names: &[String],
    overrides: &BTreeMap<Priority, String>,
) -> (BTreeMap<String, Priority>, BTreeMap<Priority, String>) {
    const LEVELS: [Priority; 5] = [
        Priority::Highest,
        Priority::High,
        Priority::Medium,
        Priority::Low,
        Priority::Lowest,
    ];
    let mut pull = map_priorities(names);
    for (level, name) in overrides {
        // Jira's own spelling, whatever the config's case.
        if let Some(n) = names.iter().find(|n| n.eq_ignore_ascii_case(name)) {
            pull.insert(n.clone(), *level);
        }
    }
    let first = |level: Priority| {
        overrides
            .get(&level)
            .and_then(|o| names.iter().find(|n| n.eq_ignore_ascii_case(o)))
            .or_else(|| names.iter().find(|n| pull.get(*n) == Some(&level)))
            .cloned()
    };
    let mut push = BTreeMap::new();
    for (i, &level) in LEVELS.iter().enumerate() {
        let near = (0..LEVELS.len()).flat_map(|d| {
            let up = i.checked_sub(d);
            let down = (d > 0).then_some(i + d).filter(|&j| j < LEVELS.len());
            up.into_iter().chain(down)
        });
        if let Some(n) = near.into_iter().find_map(|j| first(LEVELS[j])) {
            push.insert(level, n);
        }
    }
    (pull, push)
}

/// `2024-01-31T14:05:12.345+0000`: Jira's colon-less offset.
pub fn parse_time(s: &str) -> Option<Timestamp> {
    let s = s.trim();
    let fixed = match s
        .len()
        .checked_sub(5)
        .and_then(|i| s.get(i..).map(|t| (i, t)))
    {
        Some((i, tail))
            if (tail.starts_with('+') || tail.starts_with('-'))
                && tail[1..].bytes().all(|b| b.is_ascii_digit()) =>
        {
            format!("{}{}:{}", &s[..i], &tail[..3], &tail[3..])
        }
        _ => s.to_owned(),
    };
    fixed.parse::<Timestamp>().ok()
}

fn str_at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter().try_fold(v, |v, k| v.get(k))?.as_str()
}

fn names(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.get("name").and_then(Value::as_str).or_else(|| x.as_str()))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn user(v: Option<&Value>) -> Option<RemoteUser> {
    let v = v.filter(|v| !v.is_null())?;
    let id = v.get("key").or_else(|| v.get("name"))?.as_str()?.to_owned();
    Some(RemoteUser {
        display_name: v
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or(&id)
            .to_owned(),
        id,
    })
}

fn seconds(v: Option<&Value>) -> Option<SignedDuration> {
    v.and_then(Value::as_i64).map(SignedDuration::from_secs)
}

/// A sprint field's value: the open sprint's name. Older servers send each
/// sprint as a `…[name=Sprint 7,…]` string.
fn sprint(v: Option<&Value>) -> Option<String> {
    let all = v?.as_array()?;
    let name = |x: &Value| -> Option<(String, bool)> {
        if let Some(n) = x.get("name").and_then(Value::as_str) {
            let active = x.get("state").and_then(Value::as_str) == Some("active");
            return Some((n.to_owned(), active));
        }
        // `…[id=8,state=ACTIVE,name=Sprint 7, part 2,startDate=…]`: the name
        // runs to the next known field, or the closing bracket.
        let s = x.as_str()?;
        let rest = s.split_once("name=")?.1;
        let end = [
            "goal",
            "startDate",
            "endDate",
            "completeDate",
            "activatedDate",
            "sequence",
            "autoStartStop",
            "synced",
        ]
        .iter()
        .filter_map(|k| rest.find(&format!(",{k}=")))
        .min()
        .unwrap_or_else(|| rest.rfind(']').unwrap_or(rest.len()));
        Some((rest[..end].to_owned(), s.contains("state=ACTIVE")))
    };
    let sprints: Vec<(String, bool)> = all.iter().filter_map(name).collect();
    // The open sprint; an issue only in closed ones is in no sprint now.
    sprints
        .iter()
        .find(|(_, active)| *active)
        .map(|(n, _)| n.clone())
}

/// The issue `v` (a search result) as a `RemoteIssue`; `None` when it lacks
/// the identity fields.
pub fn to_remote(v: &Value, d: &Discovery, instance: &str, base_url: &str) -> Option<RemoteIssue> {
    let id = v.get("id")?.as_str()?.to_owned();
    let key = v.get("key")?.as_str()?.to_owned();
    let f = v.get("fields")?;
    let category = match str_at(f, &["status", "statusCategory", "key"]) {
        Some("done") => StatusCategory::Done,
        Some("indeterminate") => StatusCategory::InProgress,
        _ => StatusCategory::Todo,
    };
    let resolution = str_at(f, &["resolution", "name"]).map(str::to_owned);
    let resolution_kind = (category == StatusCategory::Done).then(|| match &resolution {
        Some(r)
            if d.cancelled_resolutions
                .iter()
                .any(|c| c.eq_ignore_ascii_case(r)) =>
        {
            ResolutionKind::Cancelled
        }
        _ => ResolutionKind::Done,
    });
    let priority = str_at(f, &["priority", "name"])
        .and_then(|p| d.priorities.get(p).copied())
        .unwrap_or(Priority::None);
    let assignee = user(f.get("assignee"));
    let is_mine = assignee.as_ref().is_some_and(|a| a.id == d.me);
    let custom = |id: &Option<String>| id.as_ref().and_then(|id| f.get(id));
    let mut extras = BTreeMap::new();
    for (k, val) in f.as_object()?.iter() {
        if k.starts_with("customfield_")
            && Some(k) != d.story_points.as_ref()
            && Some(k) != d.sprint.as_ref()
            && !val.is_null()
        {
            extras.insert(k.clone(), val.clone());
        }
    }
    let created = str_at(f, &["created"]).and_then(parse_time)?;
    Some(RemoteIssue {
        tracker_kind: "jira".to_owned(),
        instance_id: instance.to_owned(),
        url: format!("{}/browse/{key}", base_url.trim_end_matches('/')),
        remote_id: id,
        key,
        title: str_at(f, &["summary"]).unwrap_or_default().to_owned(),
        // Markdown, as an Item holds it; what does not convert is kept
        // (ADR-0009).
        description: crate::wiki::to_markdown(str_at(f, &["description"]).unwrap_or_default()),
        item_type: str_at(f, &["issuetype", "name"])
            .unwrap_or("Task")
            .to_owned(),
        status_category: category,
        resolution_kind,
        remote_status_name: str_at(f, &["status", "name"])
            .unwrap_or_default()
            .to_owned(),
        resolution,
        priority,
        assignee,
        reporter: user(f.get("reporter")),
        labels: names(f.get("labels")),
        components: names(f.get("components")),
        fix_versions: names(f.get("fixVersions")),
        due_date: str_at(f, &["duedate"]).and_then(|s| s.parse::<Date>().ok()),
        parent_remote_id: str_at(f, &["parent", "id"]).map(str::to_owned),
        sprint: sprint(custom(&d.sprint)),
        story_points: custom(&d.story_points).and_then(Value::as_f64),
        original_estimate: seconds(f.get("timeoriginalestimate")),
        remaining_estimate: seconds(f.get("timeestimate")),
        created_at: created,
        updated_at: str_at(f, &["updated"])
            .and_then(parse_time)
            .unwrap_or(created),
        extras,
        is_mine,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn discovery() -> Discovery {
        Discovery {
            me: "JIRAUSER1".into(),
            time_zone: "America/Los_Angeles".into(),
            priorities: map_priorities(
                &["Blocker", "Critical", "Major", "Minor", "Trivial"].map(String::from),
            ),
            story_points: Some("customfield_10002".into()),
            sprint: Some("customfield_10004".into()),
            priority_names: BTreeMap::new(),
            cancelled_resolutions: vec!["Won't Do".into()],
        }
    }

    #[test]
    fn priorities_spread_over_five_levels() {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let three = map_priorities(&names(&["High", "Medium", "Low"]));
        let push = |n: &[&str], o: &[(Priority, &str)]| {
            priority_maps(
                &names(n),
                &o.iter().map(|(l, s)| (*l, s.to_string())).collect(),
            )
            .1
        };
        let order = |m: BTreeMap<Priority, String>| m.into_values().collect::<Vec<_>>();
        // Lowest, Low, Medium, High, Highest (a BTreeMap by rank). A level
        // with no name of its own takes the nearest, above on a tie.
        assert_eq!(
            order(push(&["High", "Medium", "Low"], &[])),
            ["Low", "Medium", "Medium", "High", "High"]
        );
        assert_eq!(
            order(push(&["High", "Low"], &[])),
            ["Low", "Low", "High", "High", "High"]
        );
        assert_eq!(order(push(&["Only"], &[])), ["Only"; 5]);
        assert!(push(&[], &[]).is_empty());
        // An override holds both ways; one naming no Jira priority is ignored.
        let brme = names(&["Blocker", "Critical", "Major", "Normal", "Minor", "Trivial"]);
        let overrides: BTreeMap<_, _> = [
            (Priority::High, "major".to_owned()),
            (Priority::Low, "Nope".to_owned()),
        ]
        .into();
        let (pull, push) = priority_maps(&brme, &overrides);
        assert_eq!(push[&Priority::High], "Major");
        assert_eq!(pull["Major"], Priority::High);
        for (level, name) in &push {
            assert_eq!(
                pull[name], *level,
                "{level:?} pushes as {name}, which reads back otherwise"
            );
        }
        assert_eq!(
            (three["High"], three["Medium"], three["Low"]),
            (Priority::Highest, Priority::Medium, Priority::Lowest)
        );
        let five = map_priorities(&names(&["P1", "P2", "P3", "P4", "P5"]));
        assert_eq!(five["P2"], Priority::High);
        let seven = map_priorities(&names(&["a", "b", "c", "d", "e", "f", "g"]));
        assert_eq!(
            (seven["a"], seven["d"], seven["g"]),
            (Priority::Highest, Priority::Medium, Priority::Lowest)
        );
        assert_eq!(map_priorities(&names(&["Only"]))["Only"], Priority::Medium);
    }

    #[test]
    fn times_take_the_colon_less_offset() {
        assert_eq!(
            parse_time("2024-01-31T14:05:12.345+0000"),
            "2024-01-31T14:05:12.345Z".parse().ok()
        );
        assert_eq!(
            parse_time("2024-01-31T06:05:12.000-0800"),
            "2024-01-31T14:05:12Z".parse().ok()
        );
        assert_eq!(parse_time("nope"), None);
    }

    #[test]
    fn an_issue_maps_its_core_fields() {
        let v = json!({
            "id": "10001", "key": "PLAT-1",
            "fields": {
                "summary": "Fix the login test",
                "description": "h2. Steps",
                "issuetype": {"name": "Bug"},
                "status": {"name": "In Review", "statusCategory": {"key": "indeterminate"}},
                "priority": {"name": "Critical"},
                "assignee": {"key": "JIRAUSER1", "name": "dana", "displayName": "Dana Example"},
                "reporter": {"key": "JIRAUSER2", "displayName": "Sam Example"},
                "labels": ["auth"],
                "components": [{"id": "1", "name": "Web"}],
                "fixVersions": [{"name": "2.3"}],
                "duedate": "2026-11-06",
                "resolution": null,
                "created": "2026-10-01T09:00:00.000+0000",
                "updated": "2026-11-03T10:00:00.000+0000",
                "parent": {"id": "10000", "key": "PLAT-0"},
                "timeoriginalestimate": 7200,
                "customfield_10002": 3.0,
                "customfield_10004": ["com.atlassian.greenhopper.service.sprint.Sprint@1[id=7,state=CLOSED,name=Sprint 6]", "com.atlassian.greenhopper.service.sprint.Sprint@2[id=8,state=ACTIVE,name=Sprint 7]"],
                "customfield_10100": {"value": "Team A"}
            }
        });
        let i = to_remote(&v, &discovery(), "work-jira", "https://jira.example.com/").unwrap();
        assert_eq!((i.remote_id.as_str(), i.key.as_str()), ("10001", "PLAT-1"));
        assert_eq!(i.url, "https://jira.example.com/browse/PLAT-1");
        // Wiki markup read as Markdown (ADR-0009).
        assert_eq!(i.description, "## Steps");
        assert_eq!(
            (i.status_category, i.remote_status_name.as_str()),
            (StatusCategory::InProgress, "In Review")
        );
        assert_eq!(i.priority, Priority::High);
        assert!(i.is_mine);
        assert_eq!(i.reporter.unwrap().display_name, "Sam Example");
        assert_eq!(
            (i.components, i.fix_versions),
            (vec!["Web".to_owned()], vec!["2.3".to_owned()])
        );
        assert_eq!(i.due_date, Some(jiff::civil::date(2026, 11, 6)));
        assert_eq!(i.parent_remote_id.as_deref(), Some("10000"));
        assert_eq!(i.original_estimate, Some(SignedDuration::from_hours(2)));
        assert_eq!(
            (i.story_points, i.sprint.as_deref()),
            (Some(3.0), Some("Sprint 7"))
        );
        assert_eq!(i.extras.keys().collect::<Vec<_>>(), ["customfield_10100"]);
    }

    #[test]
    fn sprints_read_names_with_commas_and_only_the_open_one() {
        let one = |s: &str| sprint(Some(&json!([s])));
        assert_eq!(
            one("com.atlassian.greenhopper.service.sprint.Sprint@2[id=8,rapidViewId=1,state=ACTIVE,name=Sprint 7, part 2,startDate=2026-11-01T09:00:00.000Z,endDate=<null>]").as_deref(),
            Some("Sprint 7, part 2")
        );
        assert_eq!(one("Sprint@1[id=7,state=CLOSED,name=Sprint 6]"), None);
        assert_eq!(
            sprint(Some(
                &json!([{"name": "S1", "state": "closed"}, {"name": "S2", "state": "active"}])
            ))
            .as_deref(),
            Some("S2")
        );
    }

    #[test]
    fn a_resolution_decides_done_or_cancelled() {
        let mut v = json!({"id": "1", "key": "P-1", "fields": {
            "summary": "x", "status": {"name": "Closed", "statusCategory": {"key": "done"}},
            "resolution": {"name": "Won't Do"}, "created": "2026-10-01T09:00:00.000+0000"}});
        let d = discovery();
        assert_eq!(
            to_remote(&v, &d, "w", "https://j").unwrap().resolution_kind,
            Some(ResolutionKind::Cancelled)
        );
        v["fields"]["resolution"] = json!({"name": "Fixed"});
        assert_eq!(
            to_remote(&v, &d, "w", "https://j").unwrap().resolution_kind,
            Some(ResolutionKind::Done)
        );
        v["fields"]["status"]["statusCategory"]["key"] = json!("new");
        let open = to_remote(&v, &d, "w", "https://j").unwrap();
        assert_eq!(
            (open.status_category, open.resolution_kind),
            (StatusCategory::Todo, None)
        );
        assert!(!open.is_mine);
    }
}
