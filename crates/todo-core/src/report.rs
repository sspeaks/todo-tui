//! The L48 / N48 report (design 6.6): what moved in the last working days
//! and what is planned for the next. The store finds the Items; this is the
//! window, the N48 filter and the text, which need no database.

use jiff::civil::Date;

use crate::field::ItemField;
use crate::filter::{Anchor, Clause, Filter, Group, Operand, Operator, Predicate, RelativeDate};
use crate::item::{Item, PersonRef};
use crate::recurrence::Weekday;
use crate::status::Status;
use crate::value::FieldValue;

/// `[l48]` (design 8.2): how many working days each half covers, and which
/// days are working days.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportConfig {
    pub window_days: u32,
    /// Not empty.
    pub working_days: Vec<Weekday>,
}

impl Default for ReportConfig {
    fn default() -> Self {
        ReportConfig {
            window_days: 2,
            working_days: Weekday::ALL[..5].to_vec(),
        }
    }
}

/// The report's days: L48 covers `since` up to now, N48 today through
/// `until`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub since: Date,
    pub until: Date,
}

/// The longest window, in calendar days either way: a year.
const MAX_SPAN: i64 = 366;

impl ReportConfig {
    fn working(&self, d: Date) -> bool {
        self.working_days.contains(&Weekday::of(d))
    }

    /// The window on `today`. `since` is the `window_days`-th working day
    /// before today, so Monday's L48 with Monday to Friday covers Thursday
    /// and Friday (and the weekend, and Monday so far); `until` is the
    /// `window_days`-th working day after today. `since_override` replaces
    /// the start. With no working day in reach (an empty list), both are a
    /// year off.
    pub fn window(&self, today: Date, since_override: Option<Date>) -> Window {
        let step = |forward: bool| {
            let mut d = today;
            let mut left = self.window_days.max(1);
            for _ in 0..MAX_SPAN {
                d = if forward { d.tomorrow() } else { d.yesterday() }.unwrap_or(d);
                if self.working(d) {
                    left -= 1;
                    if left == 0 {
                        break;
                    }
                }
            }
            d
        };
        Window {
            since: since_override.unwrap_or_else(|| step(false)),
            until: step(true),
        }
    }
}

/// The open Items N48 plans (design 6.6): in progress, due by `until`,
/// waiting with a follow-up by then, or in this week's focus. `until` is
/// a number of days after today, as a relative date.
pub fn next_filter(days_ahead: i32) -> Filter {
    let by = || vec![Operand::from(RelativeDate::new(Anchor::Today, days_ahead))];
    Filter {
        any_of: vec![
            Clause::Predicate(Predicate::new(
                ItemField::Status,
                Operator::In,
                vec![Operand::Value(FieldValue::Status(Status::InProgress))],
            )),
            Clause::Predicate(Predicate::new(ItemField::DueDate, Operator::Le, by())),
            Clause::Group(Group {
                all_of: vec![
                    Clause::Predicate(Predicate::new(ItemField::Waiting, Operator::IsSet, vec![])),
                    Clause::Predicate(Predicate::new(
                        ItemField::WaitingFollowUpOn,
                        Operator::Le,
                        by(),
                    )),
                ],
                any_of: vec![],
            }),
            Clause::Predicate(Predicate::new(
                ItemField::FocusWeek,
                Operator::Eq,
                vec![Operand::from(RelativeDate::new(Anchor::CurrentWeek, 0))],
            )),
        ],
        ..Filter::default()
    }
}

/// One report line: an Item's title, its Jira key in parentheses when
/// linked, and who it waits on. `note` is what the store adds about its
/// subtasks ("3 of 5 subtasks done").
pub fn line_text(item: &Item, note: Option<&str>) -> String {
    let mut text = single_line(&item.core.title);
    if let Some(key) = item
        .remote_link
        .as_ref()
        .map(|l| l.key.as_str())
        .filter(|k| !k.is_empty())
    {
        text.push_str(&format!(" ({key})"));
    }
    if let Some(note) = note {
        text.push_str(&format!(": {note}"));
    }
    if let Some(w) = &item.overlay.waiting {
        let on = match &w.on {
            PersonRef::FreeText(name) => single_line(name),
            PersonRef::Person(id) => id.to_string(),
        };
        if !on.is_empty() {
            text.push_str(&format!("; waiting on {on}"));
        }
    }
    text
}

/// The report as it is pasted: `L48:` and `N48:`, a line each, `- (nothing)`
/// for an empty half.
pub fn render(last: &[String], next: &[String]) -> String {
    fn half(out: &mut String, name: &str, lines: &[String]) {
        out.push_str(name);
        out.push_str(":\n");
        if lines.is_empty() {
            out.push_str("- (nothing)\n");
        }
        for l in lines {
            out.push_str("- ");
            out.push_str(l);
            out.push('\n');
        }
    }
    let mut out = String::new();
    half(&mut out, "L48", last);
    half(&mut out, "N48", next);
    out
}

/// One line of plain text: whitespace collapsed, and control and bidi
/// formatting characters dropped, since the report goes to terminals and
/// clipboards as it is (a title may hold ESC).
fn single_line(s: &str) -> String {
    let printable: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .filter(|c| !matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'))
        .collect();
    printable.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use jiff::civil::date;

    use super::*;
    use crate::item::{CreatedVia, RemoteLink, Waiting};

    /// Monday 2026-11-02.
    const MON: Date = date(2026, 11, 2);

    #[test]
    fn the_window_counts_working_days() {
        let c = ReportConfig::default();
        // Monday: back to Thursday, on to Wednesday.
        assert_eq!(
            c.window(MON, None),
            Window {
                since: date(2026, 10, 29),
                until: date(2026, 11, 4)
            }
        );
        // Wednesday: Monday to Friday.
        assert_eq!(
            c.window(date(2026, 11, 4), None),
            Window {
                since: date(2026, 11, 2),
                until: date(2026, 11, 6)
            }
        );
        // Friday: Wednesday to Tuesday.
        assert_eq!(
            c.window(date(2026, 11, 6), None),
            Window {
                since: date(2026, 11, 4),
                until: date(2026, 11, 10)
            }
        );
        // Sunday counts back from Friday.
        assert_eq!(c.window(date(2026, 11, 1), None).since, date(2026, 10, 29));
        // One working day; every day; an override.
        let one = ReportConfig {
            window_days: 1,
            ..ReportConfig::default()
        };
        assert_eq!(one.window(MON, None).since, date(2026, 10, 30));
        let all = ReportConfig {
            window_days: 2,
            working_days: Weekday::ALL.to_vec(),
        };
        assert_eq!(all.window(MON, None).since, date(2026, 10, 31));
        let given = c.window(MON, Some(date(2026, 10, 1)));
        assert_eq!(given.since, date(2026, 10, 1));
        assert_eq!(given.until, date(2026, 11, 4));
        // No working days: a year off, never a hang.
        let none = ReportConfig {
            window_days: 2,
            working_days: vec![],
        };
        assert_eq!(
            none.window(MON, None).since,
            MON.checked_sub(jiff::Span::new().days(366)).unwrap()
        );
    }

    #[test]
    fn the_next_filter_is_valid() {
        next_filter(2).resolve(MON).unwrap();
    }

    #[test]
    fn lines_say_the_key_the_note_and_who_it_waits_on() {
        let mut item = Item::new_capture(
            crate::new_ulid(),
            "Ship  the\nmigration".into(),
            CreatedVia::Tui,
            Timestamp::UNIX_EPOCH,
        );
        assert_eq!(line_text(&item, None), "Ship the migration");
        item.remote_link = Some(RemoteLink {
            tracker_kind: "jira".into(),
            instance_id: "work".into(),
            remote_id: "1".into(),
            key: "PLAT-412".into(),
            url: String::new(),
            last_pulled_version: None,
            link_state: crate::item::LinkState::Ok,
            pinned: false,
        });
        item.overlay.waiting = Some(Waiting {
            on: PersonRef::FreeText("Sam".into()),
            reason: String::new(),
            since: MON,
            follow_up_on: None,
        });
        assert_eq!(
            line_text(&item, Some("3 of 5 subtasks done")),
            "Ship the migration (PLAT-412): 3 of 5 subtasks done; waiting on Sam"
        );
    }

    #[test]
    fn lines_drop_control_and_bidi_characters() {
        let item = Item::new_capture(
            crate::new_ulid(),
            "Ship it\u{1b}]52;c;ZXZpbA==\u{7} \u{202E}now".into(),
            CreatedVia::Tui,
            Timestamp::UNIX_EPOCH,
        );
        assert_eq!(line_text(&item, None), "Ship it ]52;c;ZXZpbA== now");
    }

    #[test]
    fn the_text_has_both_halves() {
        assert_eq!(
            render(&["a".into(), "b".into()], &[]),
            "L48:\n- a\n- b\nN48:\n- (nothing)\n"
        );
    }
}
