//! Recurring Items (design 3.8): the repeat rule grammar shared by `@`
//! tokens and the repeat prompt (design 2.3), and the schedule.
//!
//! A series is the Items sharing a `series_id`; each instance carries the
//! rule (ADR-0003). Completing an instance creates the next one with
//! [`next_instance`].

use jiff::civil::{self, Date};
use jiff::{Span, Timestamp};
use serde::{Deserialize, Serialize};

use crate::ids::ItemId;
use crate::item::{CreatedVia, Item, ItemRecurrence};
use crate::status::{Status, StatusFields, TodoRefinement};

/// A day of the week, Monday first (ISO 8601). Our own type, so that the
/// persisted serde form does not depend on jiff's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Weekday {
    /// Monday first.
    pub const ALL: [Weekday; 7] = [
        Weekday::Monday,
        Weekday::Tuesday,
        Weekday::Wednesday,
        Weekday::Thursday,
        Weekday::Friday,
        Weekday::Saturday,
        Weekday::Sunday,
    ];

    /// The weekday of `date`.
    pub fn of(date: Date) -> Self {
        date.weekday().into()
    }

    /// The weekday `name` is a prefix of, at least three letters long and
    /// in any case (`mon`, `Thursday`).
    pub fn from_name(name: &str) -> Option<Weekday> {
        weekday(&name.to_ascii_lowercase())
    }

    /// The full lowercase English name.
    pub fn name(self) -> &'static str {
        match self {
            Weekday::Monday => "monday",
            Weekday::Tuesday => "tuesday",
            Weekday::Wednesday => "wednesday",
            Weekday::Thursday => "thursday",
            Weekday::Friday => "friday",
            Weekday::Saturday => "saturday",
            Weekday::Sunday => "sunday",
        }
    }
}

impl From<civil::Weekday> for Weekday {
    fn from(w: civil::Weekday) -> Self {
        match w {
            civil::Weekday::Monday => Weekday::Monday,
            civil::Weekday::Tuesday => Weekday::Tuesday,
            civil::Weekday::Wednesday => Weekday::Wednesday,
            civil::Weekday::Thursday => Weekday::Thursday,
            civil::Weekday::Friday => Weekday::Friday,
            civil::Weekday::Saturday => Weekday::Saturday,
            civil::Weekday::Sunday => Weekday::Sunday,
        }
    }
}

/// When a recurring Item repeats. Always concrete: the forms that depend on
/// an anchor date are a [`RuleSpec`] until [`resolve`]d.
///
/// The serde form is persisted (in the Item and in events), so each tag is
/// spelled out and must never change. Build rules with the constructors,
/// which keep the invariants [`Rule::check`] tests; serde does not check
/// them, so a rule read back from storage is checked by its reader.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Rule {
    /// Every day.
    #[serde(rename = "daily")]
    Daily,
    /// On each of `days`: non-empty, sorted Monday first, without repeats.
    #[serde(rename = "weekly")]
    Weekly { days: Vec<Weekday> },
    /// On `day` (1 to 31) of every month, or the month's last day when it
    /// is shorter, recomputed per month: `31` gives Feb 28, then Mar 31.
    #[serde(rename = "monthly")]
    Monthly { day: u8 },
    /// `days` (1 to [`MAX_AFTER_DAYS`]) after the previous instance is
    /// completed, rather than on a fixed schedule.
    #[serde(rename = "after_completion")]
    AfterCompletion { days: u16 },
}

/// The longest wait an after-completion rule takes, about ten years.
pub const MAX_AFTER_DAYS: u16 = 3650;

/// A [`Rule`] that breaks an invariant; the reason is for a user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidRule(&'static str);

impl InvalidRule {
    pub fn reason(&self) -> &'static str {
        self.0
    }
}

const NO_DAYS: InvalidRule = InvalidRule("a weekly rule needs at least one day");
const UNSORTED_DAYS: InvalidRule =
    InvalidRule("weekly days must be sorted Monday first, without repeats");
const DAY_OF_MONTH: InvalidRule = InvalidRule("the day of the month must be 1 to 31");
const AFTER_DAYS: InvalidRule = InvalidRule("after completion must be 1 to 3650 days");

impl Rule {
    /// `Weekly` on `days`, sorted and without repeats.
    pub fn weekly(days: impl IntoIterator<Item = Weekday>) -> Result<Self, InvalidRule> {
        let mut days: Vec<Weekday> = days.into_iter().collect();
        days.sort_unstable();
        days.dedup();
        if days.is_empty() {
            return Err(NO_DAYS);
        }
        Ok(Rule::Weekly { days })
    }

    pub fn monthly(day: u32) -> Result<Self, InvalidRule> {
        match u8::try_from(day) {
            Ok(day @ 1..=31) => Ok(Rule::Monthly { day }),
            _ => Err(DAY_OF_MONTH),
        }
    }

    pub fn after_completion(days: u32) -> Result<Self, InvalidRule> {
        match u16::try_from(days) {
            Ok(days @ 1..=MAX_AFTER_DAYS) => Ok(Rule::AfterCompletion { days }),
            _ => Err(AFTER_DAYS),
        }
    }

    /// Whether the rule keeps the invariants the constructors establish.
    pub fn check(&self) -> Result<(), InvalidRule> {
        match self {
            Rule::Daily => Ok(()),
            Rule::Weekly { days } if days.is_empty() => Err(NO_DAYS),
            Rule::Weekly { days } if !days.is_sorted_by(|a, b| a < b) => Err(UNSORTED_DAYS),
            Rule::Weekly { .. } => Ok(()),
            Rule::Monthly { day } => Rule::monthly((*day).into()).map(drop),
            Rule::AfterCompletion { days } => Rule::after_completion((*days).into()).map(drop),
        }
    }

    /// Scheduled on fixed dates, rather than after completion.
    pub fn is_fixed(&self) -> bool {
        !matches!(self, Rule::AfterCompletion { .. })
    }

    /// The rule as an `@` token that [`parse_rule`] reads back as this rule,
    /// for `todo add` and the repeat prompt's prefill: `@daily`, `@weekdays`
    /// (Monday to Friday exactly), `@mon,thu` (three-letter names, Monday
    /// first), `@15th`, and `@2w` for a whole number of weeks after
    /// completion, else `@3d`. A resolved `@weekly` or `@monthly` shows its
    /// day, since the rule no longer knows it came from the anchor.
    pub fn token(&self) -> String {
        match self {
            Rule::Daily => "@daily".to_owned(),
            Rule::Weekly { days } if days[..] == WEEKDAYS => "@weekdays".to_owned(),
            Rule::Weekly { days } => {
                let names: Vec<&str> = days.iter().map(|d| &d.name()[..MIN_PREFIX]).collect();
                format!("@{}", names.join(","))
            }
            Rule::Monthly { day } => format!("@{day}{}", ordinal_suffix((*day).into())),
            Rule::AfterCompletion { days } if days % 7 == 0 => format!("@{}w", days / 7),
            Rule::AfterCompletion { days } => format!("@{days}d"),
        }
    }
}

/// A parsed rule before its anchor is known: `@weekly` and `@monthly` take
/// the anchor's weekday or day of the month.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RuleSpec {
    Rule(Rule),
    /// Weekly on the anchor's weekday.
    WeeklyOnAnchor,
    /// Monthly on the anchor's day of the month.
    MonthlyOnAnchor,
}

/// A repeat rule that did not parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid repeat rule {expr:?}: {reason}")]
pub struct RuleError {
    /// The expression as given.
    pub expr: String,
    /// A short human-readable reason.
    pub reason: String,
}

const NOT_A_RULE: &str = "not a repeat rule";

/// Names must be at least this long to match a weekday by prefix, as in
/// the date grammar.
const MIN_PREFIX: usize = 3;

const WEEKDAYS: [Weekday; 5] = [
    Weekday::Monday,
    Weekday::Tuesday,
    Weekday::Wednesday,
    Weekday::Thursday,
    Weekday::Friday,
];

/// Parses a repeat rule (design 2.3), with an optional leading `@`:
/// `daily`; `weekdays` (Monday to Friday); `weekly` (the anchor's weekday);
/// weekdays separated by commas, each a prefix of at least three letters
/// (`mon,thu`, repeats collapse); `monthly` (the anchor's day); a day of
/// the month from `1st` to `31st`, with its English suffix (`15th`, `22nd`,
/// not `22th` or a bare `22`, so the text reads as the rule is shown); or
/// `N` followed by `d` or `w` for `N` days or weeks after completion.
/// Case-insensitive; surrounding whitespace is ignored.
pub fn parse_rule(expr: &str) -> Result<RuleSpec, RuleError> {
    let s = expr.trim();
    let s = s.strip_prefix('@').unwrap_or(s).trim().to_ascii_lowercase();
    spec(&s).map_err(|reason| RuleError {
        expr: expr.to_owned(),
        reason,
    })
}

fn spec(s: &str) -> Result<RuleSpec, String> {
    let rule = |rule: Result<Rule, InvalidRule>| {
        rule.map(RuleSpec::Rule).map_err(|e| e.reason().to_owned())
    };
    match s {
        "" => return Err("empty repeat rule".to_owned()),
        "daily" => return Ok(RuleSpec::Rule(Rule::Daily)),
        "weekdays" => return rule(Rule::weekly(WEEKDAYS)),
        "weekly" => return Ok(RuleSpec::WeeklyOnAnchor),
        "monthly" => return Ok(RuleSpec::MonthlyOnAnchor),
        _ => {}
    }
    let digits = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if digits > 0 {
        return numbered(&s[..digits], &s[digits..]).map(RuleSpec::Rule);
    }
    let names: Vec<&str> = s.split(',').map(str::trim).collect();
    let days: Vec<Option<Weekday>> = names.iter().map(|name| weekday(name)).collect();
    if days.iter().all(Option::is_none) {
        return Err(NOT_A_RULE.to_owned());
    }
    if let Some(i) = days.iter().position(Option::is_none) {
        return Err(match names[i] {
            "" => "a day is missing between commas".to_owned(),
            name => format!("{name} is not a weekday"),
        });
    }
    rule(Rule::weekly(days.into_iter().flatten()))
}

/// The weekday `name` is a long enough prefix of.
fn weekday(name: &str) -> Option<Weekday> {
    if name.len() < MIN_PREFIX {
        return None;
    }
    Weekday::ALL
        .into_iter()
        .find(|day| day.name().starts_with(name))
}

/// `n` followed by `unit`: a day of the month, or days or weeks after
/// completion.
fn numbered(n: &str, unit: &str) -> Result<Rule, String> {
    let after = |days: Option<u32>| {
        days.ok_or(AFTER_DAYS)
            .and_then(Rule::after_completion)
            .map_err(|e| e.reason().to_owned())
    };
    // Too many digits for a u32 is out of range for every unit.
    let n: Option<u32> = n.parse().ok();
    match unit {
        "d" => after(n),
        "w" => after(n.and_then(|n| n.checked_mul(7))),
        "st" | "nd" | "rd" | "th" => {
            let Some(day @ 1..=31) = n else {
                return Err("the day of the month must be 1st to 31st".to_owned());
            };
            let suffix = ordinal_suffix(day);
            if unit != suffix {
                return Err(format!("write {day}{suffix}"));
            }
            Rule::monthly(day).map_err(|e| e.reason().to_owned())
        }
        "" => Err("a number needs d or w, or a day suffix, e.g. 3d, 2w or 15th".to_owned()),
        _ => Err(NOT_A_RULE.to_owned()),
    }
}

/// `st`, `nd`, `rd` or `th`, as in `1st`, `12th`, `22nd`.
pub fn ordinal_suffix(n: u32) -> &'static str {
    match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    }
}

/// The concrete rule of `spec` for an Item anchored on `anchor`: its due
/// date if any, else today.
pub fn resolve(spec: &RuleSpec, anchor: Date) -> Rule {
    match spec {
        RuleSpec::Rule(rule) => rule.clone(),
        RuleSpec::WeeklyOnAnchor => Rule::Weekly {
            days: vec![Weekday::of(anchor)],
        },
        // A civil day is always 1 to 31.
        RuleSpec::MonthlyOnAnchor => Rule::Monthly {
            day: anchor.day().unsigned_abs(),
        },
    }
}

/// The first date on or after `date` that `rule` schedules; `None` for an
/// after-completion rule, which has no fixed dates. Near the end of the
/// calendar, where no such date exists, [`Date::MAX`].
pub fn first_on_or_after(rule: &Rule, date: Date) -> Option<Date> {
    let found = match rule {
        Rule::AfterCompletion { .. } => return None,
        Rule::Daily => Some(date),
        Rule::Weekly { days } => (0..7)
            .filter_map(|n| date.checked_add(Span::new().days(n)).ok())
            .find(|d| days.contains(&Weekday::of(*d))),
        Rule::Monthly { day } => {
            let in_month = |first: Date| {
                let last = first.last_of_month().day();
                first.with().day((*day as i8).min(last)).build().ok()
            };
            let this = date.first_of_month();
            in_month(this).filter(|&d| d >= date).or_else(|| {
                this.checked_add(Span::new().months(1))
                    .ok()
                    .and_then(in_month)
            })
        }
    };
    Some(found.unwrap_or(Date::MAX))
}

/// The date of the instance after the one scheduled on `slot`, completed
/// `today`. A fixed rule skips the dates missed: the first scheduled date
/// after `slot` that is not before `today`. An after-completion rule waits
/// its days from `today`. Saturates at [`Date::MAX`].
pub fn next_after(rule: &Rule, slot: Date, today: Date) -> Date {
    let after = |date: Date, days: i64| {
        date.checked_add(Span::new().days(days))
            .unwrap_or(Date::MAX)
    };
    match rule {
        Rule::AfterCompletion { days } => after(today, (*days).into()),
        _ => first_on_or_after(rule, after(slot, 1).max(today)).unwrap_or(Date::MAX),
    }
}

/// The next instance of the series `completed` belongs to, created `now`
/// with id `id`, when `completed` was completed `today`; `None` when it has
/// no rule.
///
/// The instance copies the title, description, item type, priority, local
/// priority override, labels, parent and estimates, and nothing else (no
/// remote link or extras, tracker-only fields, waiting, private notes,
/// focus week, archive or relations). It is due on its date, which is also
/// its `occurrence_date`, and deferred until then unless that is today or
/// earlier. Its status is the completed Item's todo refinement, with inbox
/// becoming next. Like a capture by the user, its status and its last user
/// touch start at `now`.
pub fn next_instance(completed: &Item, id: ItemId, today: Date, now: Timestamp) -> Option<Item> {
    let recurrence = completed.overlay.recurrence.as_ref()?;
    let rule = recurrence.rule.as_ref()?;
    let slot = recurrence
        .occurrence_date
        .or(completed.core.due_date)
        .unwrap_or(today);
    let next = next_after(rule, slot, today);

    let mut item = Item::new_capture(
        id,
        completed.core.title.clone(),
        CreatedVia::Recurrence,
        now,
    );
    let core = &mut item.core;
    core.description.clone_from(&completed.core.description);
    core.item_type.clone_from(&completed.core.item_type);
    core.priority = completed.core.priority;
    core.labels.clone_from(&completed.core.labels);
    core.parent_id = completed.core.parent_id;
    core.original_estimate = completed.core.original_estimate;
    core.remaining_estimate = completed.core.remaining_estimate;
    core.due_date = Some(next);
    let status = match completed.overlay.todo_refinement {
        TodoRefinement::Inbox | TodoRefinement::Next => Status::Next,
        TodoRefinement::Someday => Status::Someday,
    };
    item.set_status_fields(StatusFields::from_status(status));
    let overlay = &mut item.overlay;
    overlay.local_priority_override = completed.overlay.local_priority_override;
    overlay.defer_until = (next > today).then_some(next);
    overlay.recurrence = Some(ItemRecurrence {
        series_id: recurrence.series_id,
        occurrence_date: Some(next),
        rule: Some(rule.clone()),
    });
    let staleness = &mut item.derived.staleness;
    staleness.status_entered_at = Some(now);
    staleness.last_user_touch_at = Some(now);
    Some(item)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use jiff::SignedDuration;
    use jiff::civil::date;
    use ulid::Ulid;

    use super::*;
    use crate::item::{
        ArchiveReason, Archived, LinkState, PersonRef, Priority, RemoteLink, Waiting,
    };
    use crate::relation::{Relation, RelationKind};
    use crate::status::ResolutionKind;

    use Weekday::*;

    /// A Thursday.
    const TODAY: Date = date(2026, 10, 22);

    fn ok(expr: &str) -> RuleSpec {
        parse_rule(expr).unwrap_or_else(|e| panic!("{expr:?}: {e}"))
    }

    fn rule(expr: &str) -> Rule {
        match ok(expr) {
            RuleSpec::Rule(rule) => rule,
            other => panic!("{expr:?}: {other:?}"),
        }
    }

    fn weekly(days: &[Weekday]) -> Rule {
        Rule::Weekly {
            days: days.to_vec(),
        }
    }

    fn reason(expr: &str) -> String {
        parse_rule(expr).expect_err(expr).reason
    }

    #[test]
    fn grammar_table() {
        let rows: &[(&str, Rule)] = &[
            ("daily", Rule::Daily),
            (
                "weekdays",
                weekly(&[Monday, Tuesday, Wednesday, Thursday, Friday]),
            ),
            ("mon", weekly(&[Monday])),
            ("monday", weekly(&[Monday])),
            ("mon,thu", weekly(&[Monday, Thursday])),
            ("thu,mon", weekly(&[Monday, Thursday])),
            ("tues,thurs", weekly(&[Tuesday, Thursday])),
            ("sat,sun", weekly(&[Saturday, Sunday])),
            ("sun,sat,fri", weekly(&[Friday, Saturday, Sunday])),
            ("wed,wednesday,wedn", weekly(&[Wednesday])),
            ("mon, thu", weekly(&[Monday, Thursday])),
            ("1st", Rule::Monthly { day: 1 }),
            ("2nd", Rule::Monthly { day: 2 }),
            ("3rd", Rule::Monthly { day: 3 }),
            ("4th", Rule::Monthly { day: 4 }),
            ("11th", Rule::Monthly { day: 11 }),
            ("12th", Rule::Monthly { day: 12 }),
            ("13th", Rule::Monthly { day: 13 }),
            ("15th", Rule::Monthly { day: 15 }),
            ("21st", Rule::Monthly { day: 21 }),
            ("22nd", Rule::Monthly { day: 22 }),
            ("23rd", Rule::Monthly { day: 23 }),
            ("31st", Rule::Monthly { day: 31 }),
            ("01st", Rule::Monthly { day: 1 }),
            ("1d", Rule::AfterCompletion { days: 1 }),
            ("3d", Rule::AfterCompletion { days: 3 }),
            ("2w", Rule::AfterCompletion { days: 14 }),
            ("3650d", Rule::AfterCompletion { days: 3650 }),
            ("521w", Rule::AfterCompletion { days: 3647 }),
            ("007d", Rule::AfterCompletion { days: 7 }),
        ];
        for (expr, want) in rows {
            assert_eq!(&rule(expr), want, "{expr}");
            assert_eq!(&rule(&format!("@{expr}")), want, "@{expr}");
            assert_eq!(want.check(), Ok(()), "{expr}");
        }
        assert_eq!(ok("weekly"), RuleSpec::WeeklyOnAnchor);
        assert_eq!(ok("@monthly"), RuleSpec::MonthlyOnAnchor);
    }

    #[test]
    fn case_and_whitespace_are_ignored() {
        assert_eq!(rule("DAILY"), Rule::Daily);
        assert_eq!(rule("@Daily"), Rule::Daily);
        assert_eq!(rule("  @weekDays \t"), rule("weekdays"));
        assert_eq!(rule("MON,Thu"), weekly(&[Monday, Thursday]));
        assert_eq!(rule("15TH"), Rule::Monthly { day: 15 });
        assert_eq!(rule("3D"), Rule::AfterCompletion { days: 3 });
        assert_eq!(rule("2W"), Rule::AfterCompletion { days: 14 });
        assert_eq!(ok("WEEKLY"), RuleSpec::WeeklyOnAnchor);
        // Whitespace after the `@` too, as `^ fri` is a date.
        assert_eq!(rule("@ daily"), Rule::Daily);
        assert_eq!(rule(" @ 15th "), Rule::Monthly { day: 15 });
        assert_eq!(rule("@\t3d"), Rule::AfterCompletion { days: 3 });
        assert_eq!(rule("@ mon"), weekly(&[Monday]));
        assert_eq!(ok("@ weekly"), RuleSpec::WeeklyOnAnchor);
    }

    #[test]
    fn errors() {
        let rows: &[(&str, &str)] = &[
            ("", "empty repeat rule"),
            ("@", "empty repeat rule"),
            ("  ", "empty repeat rule"),
            ("home", NOT_A_RULE),
            ("@home", NOT_A_RULE),
            ("weekday", NOT_A_RULE),
            ("weekends", NOT_A_RULE),
            ("yearly", NOT_A_RULE),
            ("mo", NOT_A_RULE),
            ("mondays", NOT_A_RULE),
            ("@@daily", NOT_A_RULE),
            ("daily,mon", "daily is not a weekday"),
            ("mon,fr", "fr is not a weekday"),
            ("mon,home", "home is not a weekday"),
            ("mon,", "a day is missing between commas"),
            ("mon,,thu", "a day is missing between commas"),
            (",mon", "a day is missing between commas"),
            ("0th", "the day of the month must be 1st to 31st"),
            ("32nd", "the day of the month must be 1st to 31st"),
            ("99999999999st", "the day of the month must be 1st to 31st"),
            ("1th", "write 1st"),
            ("2th", "write 2nd"),
            ("3th", "write 3rd"),
            ("4st", "write 4th"),
            ("11st", "write 11th"),
            ("12nd", "write 12th"),
            ("13rd", "write 13th"),
            ("21th", "write 21st"),
            ("22th", "write 22nd"),
            (
                "15",
                "a number needs d or w, or a day suffix, e.g. 3d, 2w or 15th",
            ),
            ("0d", "after completion must be 1 to 3650 days"),
            ("0w", "after completion must be 1 to 3650 days"),
            ("3651d", "after completion must be 1 to 3650 days"),
            ("522w", "after completion must be 1 to 3650 days"),
            ("99999999999d", "after completion must be 1 to 3650 days"),
            ("999999999w", "after completion must be 1 to 3650 days"),
            ("3m", NOT_A_RULE),
            ("3dd", NOT_A_RULE),
            ("+3d", NOT_A_RULE),
            ("15th,mon", NOT_A_RULE),
            ("fri\u{e9}", NOT_A_RULE),
        ];
        for &(expr, want) in rows {
            let err = parse_rule(expr).expect_err(expr);
            assert_eq!(err.expr, expr);
            assert_eq!(err.reason, want, "{expr}");
        }
        assert_eq!(
            parse_rule("@home").unwrap_err().to_string(),
            "invalid repeat rule \"@home\": not a repeat rule"
        );
        assert_eq!(reason("15TH,MON"), NOT_A_RULE);
    }

    #[test]
    fn ordinal_suffixes() {
        let rows = [
            (1, "st"),
            (2, "nd"),
            (3, "rd"),
            (4, "th"),
            (10, "th"),
            (11, "th"),
            (12, "th"),
            (13, "th"),
            (14, "th"),
            (20, "th"),
            (21, "st"),
            (22, "nd"),
            (23, "rd"),
            (24, "th"),
            (30, "th"),
            (31, "st"),
            (101, "st"),
            (111, "th"),
        ];
        for (n, want) in rows {
            assert_eq!(ordinal_suffix(n), want, "{n}");
        }
    }

    #[test]
    fn constructors_and_check_keep_the_invariants() {
        assert_eq!(
            Rule::weekly([Thursday, Monday, Thursday]),
            Ok(weekly(&[Monday, Thursday]))
        );
        assert_eq!(Rule::weekly([]), Err(NO_DAYS));
        assert_eq!(Rule::monthly(0), Err(DAY_OF_MONTH));
        assert_eq!(Rule::monthly(31), Ok(Rule::Monthly { day: 31 }));
        assert_eq!(Rule::monthly(32), Err(DAY_OF_MONTH));
        assert_eq!(Rule::monthly(256 + 15), Err(DAY_OF_MONTH));
        assert_eq!(Rule::after_completion(0), Err(AFTER_DAYS));
        assert_eq!(
            Rule::after_completion(3650),
            Ok(Rule::AfterCompletion { days: 3650 })
        );
        assert_eq!(Rule::after_completion(3651), Err(AFTER_DAYS));
        assert_eq!(Rule::after_completion(65536 + 3), Err(AFTER_DAYS));

        assert_eq!(weekly(&[]).check(), Err(NO_DAYS));
        assert_eq!(weekly(&[Thursday, Monday]).check(), Err(UNSORTED_DAYS));
        assert_eq!(weekly(&[Monday, Monday]).check(), Err(UNSORTED_DAYS));
        assert_eq!(Rule::Monthly { day: 0 }.check(), Err(DAY_OF_MONTH));
        assert_eq!(Rule::Monthly { day: 32 }.check(), Err(DAY_OF_MONTH));
        assert_eq!(Rule::AfterCompletion { days: 0 }.check(), Err(AFTER_DAYS));
        assert_eq!(
            Rule::AfterCompletion { days: 3651 }.check(),
            Err(AFTER_DAYS)
        );
        assert_eq!(Rule::Daily.check(), Ok(()));
        assert_eq!(
            AFTER_DAYS.to_string(),
            "after completion must be 1 to 3650 days"
        );
    }

    #[test]
    fn the_serde_form_is_fixed() {
        let rows = [
            (Rule::Daily, r#""daily""#),
            (
                weekly(&[Monday, Sunday]),
                r#"{"weekly":{"days":["monday","sunday"]}}"#,
            ),
            (Rule::Monthly { day: 31 }, r#"{"monthly":{"day":31}}"#),
            (
                Rule::AfterCompletion { days: 14 },
                r#"{"after_completion":{"days":14}}"#,
            ),
        ];
        for (rule, json) in rows {
            assert_eq!(serde_json::to_string(&rule).unwrap(), json);
            assert_eq!(serde_json::from_str::<Rule>(json).unwrap(), rule);
        }
        let all = serde_json::to_string(&Weekday::ALL).unwrap();
        assert_eq!(
            all,
            r#"["monday","tuesday","wednesday","thursday","friday","saturday","sunday"]"#
        );
    }

    #[test]
    fn token_reads_back_as_the_rule() {
        let rows: &[(Rule, &str)] = &[
            (Rule::Daily, "@daily"),
            (
                weekly(&[Monday, Tuesday, Wednesday, Thursday, Friday]),
                "@weekdays",
            ),
            (weekly(&[Monday, Thursday]), "@mon,thu"),
            (weekly(&[Thursday]), "@thu"),
            (weekly(&[Friday, Saturday, Sunday]), "@fri,sat,sun"),
            (
                weekly(&[Monday, Tuesday, Wednesday, Thursday, Friday, Saturday]),
                "@mon,tue,wed,thu,fri,sat",
            ),
            (weekly(&Weekday::ALL), "@mon,tue,wed,thu,fri,sat,sun"),
            (Rule::Monthly { day: 1 }, "@1st"),
            (Rule::Monthly { day: 2 }, "@2nd"),
            (Rule::Monthly { day: 3 }, "@3rd"),
            (Rule::Monthly { day: 11 }, "@11th"),
            (Rule::Monthly { day: 15 }, "@15th"),
            (Rule::Monthly { day: 22 }, "@22nd"),
            (Rule::Monthly { day: 31 }, "@31st"),
            (Rule::AfterCompletion { days: 1 }, "@1d"),
            (Rule::AfterCompletion { days: 3 }, "@3d"),
            (Rule::AfterCompletion { days: 7 }, "@1w"),
            (Rule::AfterCompletion { days: 14 }, "@2w"),
            (Rule::AfterCompletion { days: 15 }, "@15d"),
            (Rule::AfterCompletion { days: 3647 }, "@521w"),
            (Rule::AfterCompletion { days: 3650 }, "@3650d"),
        ];
        for (want, token) in rows {
            assert_eq!(want.token(), *token, "{want:?}");
            assert_eq!(&rule(token), want, "{token}");
        }
        // Every rule of every kind reads back.
        let mut every = vec![Rule::Daily];
        every.extend((1..=31).map(|day| Rule::Monthly { day }));
        every.extend((1..=MAX_AFTER_DAYS).map(|days| Rule::AfterCompletion { days }));
        every.extend((1..128u8).map(|mask| {
            let days = Weekday::ALL
                .into_iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, d)| d);
            Rule::weekly(days).unwrap()
        }));
        for want in every {
            assert_eq!(rule(&want.token()), want, "{}", want.token());
        }
        // The anchor forms show what they resolved to.
        let friday = date(2026, 10, 23);
        assert_eq!(resolve(&ok("@weekly"), friday).token(), "@fri");
        assert_eq!(resolve(&ok("@monthly"), friday).token(), "@23rd");
    }

    #[test]
    fn resolve_takes_the_anchor() {
        let friday = date(2026, 10, 23);
        assert_eq!(
            resolve(&RuleSpec::WeeklyOnAnchor, friday),
            weekly(&[Friday])
        );
        assert_eq!(
            resolve(&RuleSpec::MonthlyOnAnchor, date(2026, 1, 31)),
            Rule::Monthly { day: 31 }
        );
        assert_eq!(
            resolve(&RuleSpec::MonthlyOnAnchor, date(2026, 2, 1)),
            Rule::Monthly { day: 1 }
        );
        assert_eq!(resolve(&RuleSpec::Rule(Rule::Daily), friday), Rule::Daily);
        for d in 0..7 {
            let anchor = TODAY.checked_add(Span::new().days(d)).unwrap();
            let resolved = resolve(&RuleSpec::WeeklyOnAnchor, anchor);
            assert_eq!(first_on_or_after(&resolved, anchor), Some(anchor));
        }
    }

    #[test]
    fn first_on_or_after_each_rule() {
        let rows = [
            (Rule::Daily, TODAY, Some(TODAY)),
            (weekly(&[Thursday]), TODAY, Some(TODAY)),
            (weekly(&[Friday]), TODAY, Some(date(2026, 10, 23))),
            // The week wraps.
            (weekly(&[Monday]), TODAY, Some(date(2026, 10, 26))),
            (
                weekly(&[Monday, Wednesday]),
                TODAY,
                Some(date(2026, 10, 26)),
            ),
            (weekly(&[Wednesday]), TODAY, Some(date(2026, 10, 28))),
            (
                weekly(&[Sunday]),
                date(2026, 12, 28),
                Some(date(2027, 1, 3)),
            ),
            (Rule::Monthly { day: 22 }, TODAY, Some(TODAY)),
            (Rule::Monthly { day: 25 }, TODAY, Some(date(2026, 10, 25))),
            (Rule::Monthly { day: 1 }, TODAY, Some(date(2026, 11, 1))),
            (Rule::Monthly { day: 31 }, TODAY, Some(date(2026, 10, 31))),
            (
                Rule::Monthly { day: 31 },
                date(2026, 11, 5),
                Some(date(2026, 11, 30)),
            ),
            (
                Rule::Monthly { day: 15 },
                date(2026, 12, 20),
                Some(date(2027, 1, 15)),
            ),
            (
                Rule::Monthly { day: 30 },
                date(2026, 2, 1),
                Some(date(2026, 2, 28)),
            ),
            (
                Rule::Monthly { day: 29 },
                date(2028, 2, 1),
                Some(date(2028, 2, 29)),
            ),
            (Rule::AfterCompletion { days: 3 }, TODAY, None),
        ];
        for (rule, from, want) in rows {
            assert_eq!(first_on_or_after(&rule, from), want, "{rule:?} from {from}");
        }
    }

    /// Successive instances of `rule` from `slot`, each completed on its day.
    fn series(rule: &Rule, slot: Date, n: usize) -> Vec<Date> {
        std::iter::successors(Some(slot), |&slot| Some(next_after(rule, slot, slot)))
            .skip(1)
            .take(n)
            .collect()
    }

    #[test]
    fn months_clamp_to_their_last_day_then_recover() {
        let day31 = Rule::Monthly { day: 31 };
        assert_eq!(
            series(&day31, date(2026, 12, 31), 5),
            [
                date(2027, 1, 31),
                date(2027, 2, 28),
                date(2027, 3, 31),
                date(2027, 4, 30),
                date(2027, 5, 31),
            ]
        );
        // A leap year.
        assert_eq!(
            series(&day31, date(2028, 1, 31), 2),
            [date(2028, 2, 29), date(2028, 3, 31)]
        );
        let day29 = Rule::Monthly { day: 29 };
        assert_eq!(
            series(&day29, date(2027, 1, 29), 2),
            [date(2027, 2, 28), date(2027, 3, 29)]
        );
        assert_eq!(
            series(&day29, date(2028, 1, 29), 2),
            [date(2028, 2, 29), date(2028, 3, 29)]
        );
        // 2100 is not a leap year.
        assert_eq!(
            series(&Rule::Monthly { day: 30 }, date(2100, 1, 30), 2),
            [date(2100, 2, 28), date(2100, 3, 30)]
        );
        // Year end.
        assert_eq!(
            series(&Rule::Monthly { day: 15 }, date(2026, 12, 15), 1),
            [date(2027, 1, 15)]
        );
    }

    #[test]
    fn weeks_wrap() {
        let mon_thu = weekly(&[Monday, Thursday]);
        assert_eq!(
            series(&mon_thu, date(2026, 10, 22), 4),
            [
                date(2026, 10, 26),
                date(2026, 10, 29),
                date(2026, 11, 2),
                date(2026, 11, 5),
            ]
        );
        assert_eq!(
            series(&weekly(&[Sunday]), date(2026, 12, 27), 2),
            [date(2027, 1, 3), date(2027, 1, 10)]
        );
        assert_eq!(
            series(&Rule::Daily, date(2026, 12, 30), 3),
            [date(2026, 12, 31), date(2027, 1, 1), date(2027, 1, 2)]
        );
    }

    #[test]
    fn weekdays_skip_the_weekend() {
        let weekdays = rule("weekdays");
        let friday = date(2026, 10, 23);
        assert_eq!(
            series(&weekdays, friday, 2),
            [date(2026, 10, 26), date(2026, 10, 27)]
        );
        // Completed on Saturday or Sunday: the next is Monday.
        for weekend in [date(2026, 10, 24), date(2026, 10, 25)] {
            assert_eq!(next_after(&weekdays, friday, weekend), date(2026, 10, 26));
        }
        assert_eq!(
            first_on_or_after(&weekdays, date(2026, 10, 24)),
            Some(date(2026, 10, 26))
        );
    }

    #[test]
    fn missed_dates_are_skipped() {
        // Due Monday Oct 5, completed weeks late on Thursday Oct 22.
        let slot = date(2026, 10, 5);
        assert_eq!(
            next_after(&weekly(&[Monday]), slot, TODAY),
            date(2026, 10, 26)
        );
        assert_eq!(
            next_after(&weekly(&[Monday, Thursday]), slot, TODAY),
            TODAY,
            "today itself is scheduled"
        );
        assert_eq!(next_after(&Rule::Daily, slot, TODAY), TODAY);
        assert_eq!(
            next_after(&Rule::Monthly { day: 5 }, slot, TODAY),
            date(2026, 11, 5)
        );
        assert_eq!(
            next_after(&Rule::Monthly { day: 31 }, date(2026, 1, 31), TODAY),
            date(2026, 10, 31)
        );
        // Completed early: still after the slot, not before today.
        let early = date(2026, 10, 20);
        assert_eq!(next_after(&Rule::Daily, TODAY, early), date(2026, 10, 23));
        assert_eq!(
            next_after(&weekly(&[Thursday]), TODAY, early),
            date(2026, 10, 29)
        );
        // After completion counts from today, whatever the slot.
        assert_eq!(
            next_after(&Rule::AfterCompletion { days: 3 }, slot, TODAY),
            date(2026, 10, 25)
        );
        assert_eq!(
            next_after(&Rule::AfterCompletion { days: 3 }, date(2027, 1, 1), TODAY),
            date(2026, 10, 25)
        );
    }

    #[test]
    fn the_end_of_the_calendar_saturates() {
        assert_eq!(next_after(&Rule::Daily, Date::MAX, TODAY), Date::MAX);
        assert_eq!(
            next_after(&Rule::AfterCompletion { days: 10 }, TODAY, Date::MAX),
            Date::MAX
        );
        assert_eq!(
            first_on_or_after(&Rule::Monthly { day: 1 }, date(9999, 12, 2)),
            Some(Date::MAX)
        );
        assert_eq!(
            first_on_or_after(&weekly(&[Monday]), date(9999, 12, 28)),
            Some(Date::MAX)
        );
    }

    fn id(n: u128) -> Ulid {
        Ulid::from(n)
    }

    fn ts(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).unwrap()
    }

    /// A completed instance with every field set.
    fn completed(rule: Rule, occurrence_date: Option<Date>) -> Item {
        let mut item = Item::new_capture(id(1), "water plants".into(), CreatedVia::Tui, ts(1_000));
        item.revision = 9;
        item.updated_at = ts(2_000);
        item.remote_link = Some(RemoteLink {
            tracker_kind: "jira".into(),
            instance_id: "work-jira".into(),
            remote_id: "10001".into(),
            key: "PLAT-530".into(),
            url: "https://jira.example.com/browse/PLAT-530".into(),
            last_pulled_version: None,
            link_state: LinkState::Ok,
            pinned: false,
        });
        let core = &mut item.core;
        core.description = "the ferns too".into();
        core.item_type = "chore".into();
        core.remote_status_name = Some("Done".into());
        core.priority = Priority::High;
        core.assignee = Some(PersonRef::Person(id(2)));
        core.reporter = Some(PersonRef::Person(id(3)));
        core.labels = vec!["home".into(), "garden".into()];
        core.components = vec!["api".into()];
        core.fix_versions = vec!["1.2".into()];
        core.due_date = Some(date(2026, 10, 19));
        core.parent_id = Some(id(4));
        core.sprint = Some("Sprint 7".into());
        core.story_points = Some(1.0);
        core.original_estimate = Some(SignedDuration::from_mins(30));
        core.remaining_estimate = Some(SignedDuration::from_mins(10));
        core.resolution = Some("Fixed".into());
        core.remote_created_at = Some(ts(500));
        core.remote_updated_at = Some(ts(600));
        item.set_status_fields(StatusFields::from_status(Status::Next).with_status(Status::Done));
        let overlay = &mut item.overlay;
        overlay.waiting = Some(Waiting {
            on: PersonRef::FreeText("dana".into()),
            reason: "hose".into(),
            since: date(2026, 10, 1),
            follow_up_on: None,
        });
        overlay.defer_until = Some(date(2026, 10, 18));
        overlay.private_notes = "notes".into();
        overlay.focus_week = Some("2026-W43".into());
        overlay.local_priority_override = Some(Priority::Low);
        overlay.archived = Some(Archived {
            at: ts(1_500),
            reason: ArchiveReason::User,
        });
        overlay.recurrence = Some(ItemRecurrence {
            series_id: id(7),
            occurrence_date,
            rule: Some(rule),
        });
        item.remote_extras = BTreeMap::from([("customfield_1".into(), serde_json::json!(1))]);
        let derived = &mut item.derived;
        derived.is_mine = true;
        derived.scopes = vec!["mine".into()];
        derived.subtasks_total = 2;
        derived.subtasks_done = 1;
        derived.parent_title = Some("garden".into());
        derived.staleness.last_user_touch_at = Some(ts(2_000));
        item.relations = vec![Relation::new(id(1), id(5), RelationKind::Blocks).unwrap()];
        item
    }

    #[test]
    fn next_instance_copies_what_the_plan_lists() {
        let now = ts(9_000);
        let done = completed(weekly(&[Monday, Thursday]), Some(date(2026, 10, 19)));
        let next = next_instance(&done, id(10), TODAY, now).unwrap();

        let mut want =
            Item::new_capture(id(10), "water plants".into(), CreatedVia::Recurrence, now);
        want.core.description = "the ferns too".into();
        want.core.item_type = "chore".into();
        want.core.priority = Priority::High;
        want.core.labels = vec!["home".into(), "garden".into()];
        want.core.parent_id = Some(id(4));
        want.core.original_estimate = Some(SignedDuration::from_mins(30));
        want.core.remaining_estimate = Some(SignedDuration::from_mins(10));
        // Due Monday Oct 19, completed Thursday Oct 22, which is scheduled.
        want.core.due_date = Some(TODAY);
        want.set_status_fields(StatusFields::from_status(Status::Next));
        want.overlay.local_priority_override = Some(Priority::Low);
        want.overlay.recurrence = Some(ItemRecurrence {
            series_id: id(7),
            occurrence_date: Some(TODAY),
            rule: Some(weekly(&[Monday, Thursday])),
        });
        want.derived.staleness.status_entered_at = Some(now);
        want.derived.staleness.last_user_touch_at = Some(now);
        assert_eq!(next, want);
        // Not deferred: it is due today.
        assert_eq!(next.overlay.defer_until, None);
        assert_eq!(next.revision, 0);
        assert!(next.relations.is_empty());
        assert!(next.remote_link.is_none());
    }

    #[test]
    fn next_instance_is_deferred_until_a_later_date() {
        let done = completed(weekly(&[Monday]), Some(date(2026, 10, 19)));
        let next = next_instance(&done, id(10), TODAY, ts(9_000)).unwrap();
        assert_eq!(next.core.due_date, Some(date(2026, 10, 26)));
        assert_eq!(next.overlay.defer_until, Some(date(2026, 10, 26)));
        let recurrence = next.overlay.recurrence.unwrap();
        assert_eq!(recurrence.occurrence_date, Some(date(2026, 10, 26)));
    }

    #[test]
    fn the_slot_is_the_occurrence_then_the_due_date_then_today() {
        let rule = Rule::Daily;
        // occurrence_date wins over due_date.
        let done = completed(rule.clone(), Some(date(2026, 10, 25)));
        let next = next_instance(&done, id(10), TODAY, ts(9_000)).unwrap();
        assert_eq!(next.core.due_date, Some(date(2026, 10, 26)));
        // Without one, the due date (Oct 19, missed: today).
        let done = completed(rule.clone(), None);
        let next = next_instance(&done, id(10), TODAY, ts(9_000)).unwrap();
        assert_eq!(next.core.due_date, Some(TODAY));
        // Without either, today: tomorrow.
        let mut done = completed(rule, None);
        done.core.due_date = None;
        let next = next_instance(&done, id(10), TODAY, ts(9_000)).unwrap();
        assert_eq!(next.core.due_date, Some(date(2026, 10, 23)));
        assert_eq!(next.overlay.defer_until, Some(date(2026, 10, 23)));
    }

    #[test]
    fn after_completion_without_a_due_date() {
        let mut done = completed(Rule::AfterCompletion { days: 3 }, None);
        done.core.due_date = None;
        let next = next_instance(&done, id(10), TODAY, ts(9_000)).unwrap();
        assert_eq!(next.core.due_date, Some(date(2026, 10, 25)));
        assert_eq!(next.overlay.defer_until, Some(date(2026, 10, 25)));
        let recurrence = next.overlay.recurrence.unwrap();
        assert_eq!(recurrence.occurrence_date, Some(date(2026, 10, 25)));
        assert_eq!(recurrence.rule, Some(Rule::AfterCompletion { days: 3 }));
        assert_eq!(recurrence.series_id, id(7));
    }

    #[test]
    fn next_instance_status_follows_the_refinement() {
        let rows = [
            (TodoRefinement::Inbox, Status::Next),
            (TodoRefinement::Next, Status::Next),
            (TodoRefinement::Someday, Status::Someday),
        ];
        for (refinement, want) in rows {
            let mut done = completed(Rule::Daily, None);
            done.overlay.todo_refinement = refinement;
            for closed in [Status::Done, Status::Cancelled] {
                done.set_status_fields(done.status_fields().with_status(closed));
                let next = next_instance(&done, id(10), TODAY, ts(9_000)).unwrap();
                assert_eq!(next.status(), want, "{refinement:?} {closed:?}");
                assert_eq!(next.core.resolution_kind, None);
            }
        }
        let mut cancelled = completed(Rule::Daily, None);
        cancelled.core.resolution_kind = Some(ResolutionKind::Cancelled);
        assert!(next_instance(&cancelled, id(10), TODAY, ts(9_000)).is_some());
    }

    #[test]
    fn no_rule_no_instance() {
        let mut legacy = completed(Rule::Daily, None);
        legacy.overlay.recurrence.as_mut().unwrap().rule = None;
        assert_eq!(next_instance(&legacy, id(10), TODAY, ts(9_000)), None);
        legacy.overlay.recurrence = None;
        assert_eq!(next_instance(&legacy, id(10), TODAY, ts(9_000)), None);
    }
}
