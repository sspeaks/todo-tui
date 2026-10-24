//! The date grammar shared by `^` tokens and date prompts (design 2.3).

use jiff::Span;
use jiff::civil::{Date, Weekday};

/// A date expression that did not resolve.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid date {expr:?}: {reason}")]
pub struct DateError {
    /// The expression as given.
    pub expr: String,
    /// A short human-readable reason.
    pub reason: String,
}

const WEEKDAYS: [(&str, Weekday); 7] = [
    ("monday", Weekday::Monday),
    ("tuesday", Weekday::Tuesday),
    ("wednesday", Weekday::Wednesday),
    ("thursday", Weekday::Thursday),
    ("friday", Weekday::Friday),
    ("saturday", Weekday::Saturday),
    ("sunday", Weekday::Sunday),
];

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// The most days each month can have (February in a leap year).
const MAX_DAYS: [i8; 12] = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// Names must be at least this long to match a weekday or month by prefix.
const MIN_PREFIX: usize = 3;

const NOT_A_DATE: &str = "not a date";
const OUT_OF_RANGE: &str = "out of range";

/// Resolves a date expression against `today` (design 2.3): `today`/`tod`,
/// `tomorrow`/`tom`, a weekday (next occurrence strictly after today),
/// `+Nd`/`+Nw`/`+Nm`, `eow`, `eom`, `YYYY-MM-DD`, or a month and day such as
/// `nov2` (the first such date on or after today). Case-insensitive.
pub fn parse_date(expr: &str, today: Date) -> Result<Date, DateError> {
    resolve(&expr.trim().to_ascii_lowercase(), today).map_err(|reason| DateError {
        expr: expr.to_owned(),
        reason,
    })
}

fn resolve(s: &str, today: Date) -> Result<Date, String> {
    let out_of_range = |_| OUT_OF_RANGE.to_owned();
    match s {
        "" => return Err("empty date".to_owned()),
        "today" | "tod" => return Ok(today),
        "tomorrow" | "tom" => return today.tomorrow().map_err(out_of_range),
        "eow" if today.weekday() == Weekday::Friday => return Ok(today),
        "eow" => return today.nth_weekday(1, Weekday::Friday).map_err(out_of_range),
        "eom" => return Ok(today.last_of_month()),
        _ => {}
    }
    if let Some(rel) = s.strip_prefix('+') {
        return relative(rel, today);
    }
    if let Some(date) = absolute(s) {
        return date;
    }
    let letters = s
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(s.len());
    let (name, digits) = s.split_at(letters);
    if name.len() < MIN_PREFIX {
        return Err(NOT_A_DATE.to_owned());
    }
    if digits.is_empty() {
        if let Some(&(_, weekday)) = WEEKDAYS.iter().find(|(full, _)| full.starts_with(name)) {
            return today.nth_weekday(1, weekday).map_err(out_of_range);
        }
        if month_index(name).is_some() {
            return Err("a month needs a day, e.g. nov2".to_owned());
        }
        return Err(NOT_A_DATE.to_owned());
    }
    match month_index(name) {
        Some(index) if digits.len() <= 2 && digits.bytes().all(|b| b.is_ascii_digit()) => {
            month_day(
                index,
                digits.parse().map_err(|_| NOT_A_DATE.to_owned())?,
                today,
            )
        }
        _ => Err(NOT_A_DATE.to_owned()),
    }
}

/// `N` followed by `d`, `w` or `m`, after the `+`.
fn relative(s: &str, today: Date) -> Result<Date, String> {
    let expected = || "expected +N followed by d, w or m".to_owned();
    let Some((last, unit)) = s.char_indices().next_back() else {
        return Err(expected());
    };
    let n = &s[..last];
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return Err(expected());
    }
    let n: i64 = n.parse().map_err(|_| OUT_OF_RANGE.to_owned())?;
    let span = match unit {
        'd' => Span::new().try_days(n),
        'w' => Span::new().try_weeks(n),
        'm' => Span::new().try_months(n),
        _ => return Err(expected()),
    };
    span.and_then(|span| today.checked_add(span))
        .map_err(|_| OUT_OF_RANGE.to_owned())
}

/// `YYYY-MM-DD`, or `None` when `s` does not have that shape.
fn absolute(s: &str) -> Option<Result<Date, String>> {
    let b = s.as_bytes();
    let shaped = b.len() == 10
        && b.iter().enumerate().all(|(i, &c)| {
            if i == 4 || i == 7 {
                c == b'-'
            } else {
                c.is_ascii_digit()
            }
        });
    if !shaped {
        return None;
    }
    // The digit runs are short enough that these parses cannot fail.
    let (year, month, day) = (
        s[0..4].parse().ok()?,
        s[5..7].parse().ok()?,
        s[8..10].parse().ok()?,
    );
    Some(Date::new(year, month, day).map_err(|_| "no such date".to_owned()))
}

/// The index into [`MONTHS`] of the month whose name starts with `name`.
fn month_index(name: &str) -> Option<usize> {
    MONTHS.iter().position(|full| full.starts_with(name))
}

/// The first date on or after `today` with that month (an index into
/// [`MONTHS`]) and day, looking far enough ahead to reach the next leap year
/// for February 29.
fn month_day(index: usize, day: i8, today: Date) -> Result<Date, String> {
    let month = index as i8 + 1;
    if !(1..=MAX_DAYS[index]).contains(&day) {
        return Err(format!("{} has no day {day}", MONTHS[index]));
    }
    // Leap years can be 8 years apart (2096 -> 2104).
    (today.year()..=today.year().saturating_add(8))
        .filter_map(|year| Date::new(year, month, day).ok())
        .find(|&date| date >= today)
        .ok_or_else(|| OUT_OF_RANGE.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    /// A Thursday.
    const TODAY: Date = date(2026, 10, 22);

    fn ok(expr: &str, today: Date) -> Date {
        parse_date(expr, today).unwrap_or_else(|e| panic!("{expr:?}: {e}"))
    }

    #[test]
    fn grammar_table() {
        let rows: &[(&str, Date)] = &[
            ("today", TODAY),
            ("tod", TODAY),
            ("tomorrow", date(2026, 10, 23)),
            ("tom", date(2026, 10, 23)),
            ("mon", date(2026, 10, 26)),
            ("monday", date(2026, 10, 26)),
            ("tue", date(2026, 10, 27)),
            ("tues", date(2026, 10, 27)),
            ("tuesday", date(2026, 10, 27)),
            ("wed", date(2026, 10, 28)),
            ("wednes", date(2026, 10, 28)),
            ("thu", date(2026, 10, 29)),
            ("thur", date(2026, 10, 29)),
            ("thurs", date(2026, 10, 29)),
            ("thursday", date(2026, 10, 29)),
            ("fri", date(2026, 10, 23)),
            ("friday", date(2026, 10, 23)),
            ("sat", date(2026, 10, 24)),
            ("saturday", date(2026, 10, 24)),
            ("sun", date(2026, 10, 25)),
            ("sunday", date(2026, 10, 25)),
            ("+0d", TODAY),
            ("+3d", date(2026, 10, 25)),
            ("+10d", date(2026, 11, 1)),
            ("+0w", TODAY),
            ("+2w", date(2026, 11, 5)),
            ("+0m", TODAY),
            ("+1m", date(2026, 11, 22)),
            ("+3m", date(2027, 1, 22)),
            ("+12m", date(2027, 10, 22)),
            ("+007d", date(2026, 10, 29)),
            ("eow", date(2026, 10, 23)),
            ("eom", date(2026, 10, 31)),
            ("2026-10-24", date(2026, 10, 24)),
            ("2024-02-29", date(2024, 2, 29)),
            ("1999-01-01", date(1999, 1, 1)),
            ("nov2", date(2026, 11, 2)),
            ("nov02", date(2026, 11, 2)),
            ("november2", date(2026, 11, 2)),
            ("oct22", TODAY),
            ("oct23", date(2026, 10, 23)),
            ("oct21", date(2027, 10, 21)),
            ("jan1", date(2027, 1, 1)),
            ("sep30", date(2027, 9, 30)),
            ("sept30", date(2027, 9, 30)),
            ("september30", date(2027, 9, 30)),
            ("dec31", date(2026, 12, 31)),
            ("feb29", date(2028, 2, 29)),
            ("mar1", date(2027, 3, 1)),
            ("may5", date(2027, 5, 5)),
        ];
        for &(expr, want) in rows {
            assert_eq!(ok(expr, TODAY), want, "{expr}");
        }
    }

    #[test]
    fn case_and_whitespace_are_ignored() {
        for expr in [
            "TODAY",
            "Tom",
            "FRI",
            "  fri  ",
            "\tFriday\n",
            "Nov2",
            "EOW",
            "+3D",
        ] {
            assert!(parse_date(expr, TODAY).is_ok(), "{expr}");
        }
        assert_eq!(ok("FRIDAY", TODAY), ok("fri", TODAY));
        assert_eq!(ok("NOV02", TODAY), date(2026, 11, 2));
        assert_eq!(ok("+2W", TODAY), date(2026, 11, 5));
    }

    #[test]
    fn weekday_is_strictly_after_today() {
        let monday = date(2026, 10, 19);
        for offset in 0..7 {
            let today = monday.checked_add(Span::new().days(offset)).unwrap();
            for (name, weekday) in WEEKDAYS {
                let got = ok(&name[..3], today);
                assert_eq!(got.weekday(), weekday, "{name} from {today}");
                let ahead = today.until(got).unwrap().get_days();
                assert!((1..=7).contains(&ahead), "{name} from {today}: {ahead}");
            }
        }
        // `fri` on a Friday is next week's Friday.
        assert_eq!(ok("fri", date(2026, 10, 23)), date(2026, 10, 30));
        assert_eq!(ok("thu", TODAY), date(2026, 10, 29));
    }

    #[test]
    fn eow_on_each_weekday() {
        let rows = [
            (date(2026, 10, 19), date(2026, 10, 23)),
            (date(2026, 10, 20), date(2026, 10, 23)),
            (date(2026, 10, 21), date(2026, 10, 23)),
            (date(2026, 10, 22), date(2026, 10, 23)),
            (date(2026, 10, 23), date(2026, 10, 23)),
            (date(2026, 10, 24), date(2026, 10, 30)),
            (date(2026, 10, 25), date(2026, 10, 30)),
        ];
        for (today, want) in rows {
            assert_eq!(ok("eow", today), want, "eow from {today}");
        }
    }

    #[test]
    fn eom() {
        assert_eq!(ok("eom", date(2026, 2, 10)), date(2026, 2, 28));
        assert_eq!(ok("eom", date(2028, 2, 10)), date(2028, 2, 29));
        assert_eq!(ok("eom", date(2026, 4, 1)), date(2026, 4, 30));
        assert_eq!(ok("eom", date(2026, 12, 31)), date(2026, 12, 31));
    }

    #[test]
    fn months_clamp_to_the_end_of_the_month() {
        let rows = [
            (date(2026, 1, 31), "+1m", date(2026, 2, 28)),
            (date(2028, 1, 31), "+1m", date(2028, 2, 29)),
            (date(2026, 3, 31), "+1m", date(2026, 4, 30)),
            (date(2026, 8, 31), "+6m", date(2027, 2, 28)),
            (date(2028, 2, 29), "+12m", date(2029, 2, 28)),
            (date(2028, 2, 29), "+48m", date(2032, 2, 29)),
            (date(2026, 12, 15), "+1m", date(2027, 1, 15)),
        ];
        for (today, expr, want) in rows {
            assert_eq!(ok(expr, today), want, "{expr} from {today}");
        }
    }

    #[test]
    fn month_day_this_year_next_year_and_leap_days() {
        assert_eq!(ok("feb29", date(2028, 2, 29)), date(2028, 2, 29));
        assert_eq!(ok("feb29", date(2028, 3, 1)), date(2032, 2, 29));
        // 2100 is not a leap year.
        assert_eq!(ok("feb29", date(2097, 1, 1)), date(2104, 2, 29));
        assert_eq!(ok("dec31", date(2026, 12, 31)), date(2026, 12, 31));
        assert_eq!(ok("jan1", date(2026, 12, 31)), date(2027, 1, 1));
        assert_eq!(ok("feb28", date(2026, 3, 1)), date(2027, 2, 28));
    }

    #[test]
    fn errors() {
        let rows: &[(&str, &str)] = &[
            ("", "empty date"),
            ("   ", "empty date"),
            ("fr", NOT_A_DATE),
            ("fridays", NOT_A_DATE),
            ("someday", NOT_A_DATE),
            ("next friday", NOT_A_DATE),
            ("abc", NOT_A_DATE),
            ("frié", NOT_A_DATE),
            ("é", NOT_A_DATE),
            ("2026/10/24", NOT_A_DATE),
            ("2026-1-01", NOT_A_DATE),
            ("26-10-24", NOT_A_DATE),
            ("20261024", NOT_A_DATE),
            ("2026-10-24x", NOT_A_DATE),
            ("2026-02-30", "no such date"),
            ("2026-13-01", "no such date"),
            ("2026-00-10", "no such date"),
            ("2025-02-29", "no such date"),
            ("nov", "a month needs a day, e.g. nov2"),
            ("mon2", NOT_A_DATE),
            ("no2", NOT_A_DATE),
            ("nov123", NOT_A_DATE),
            ("nov2x", NOT_A_DATE),
            ("nov-2", NOT_A_DATE),
            ("feb30", "february has no day 30"),
            ("apr31", "april has no day 31"),
            ("jan0", "january has no day 0"),
            ("jan00", "january has no day 0"),
            ("jan32", "january has no day 32"),
            ("+", "expected +N followed by d, w or m"),
            ("+d", "expected +N followed by d, w or m"),
            ("+3", "expected +N followed by d, w or m"),
            ("+3x", "expected +N followed by d, w or m"),
            ("+-3d", "expected +N followed by d, w or m"),
            ("+3.5d", "expected +N followed by d, w or m"),
            ("+ 3d", "expected +N followed by d, w or m"),
            ("+5é", "expected +N followed by d, w or m"),
            ("+99999999999999999999999d", OUT_OF_RANGE),
            ("+9999999999d", OUT_OF_RANGE),
            ("+99999999w", OUT_OF_RANGE),
            ("+999999m", OUT_OF_RANGE),
            ("+3000000d", OUT_OF_RANGE),
        ];
        for &(expr, reason) in rows {
            let err = parse_date(expr, TODAY).expect_err(expr);
            assert_eq!(err.expr, expr);
            assert_eq!(err.reason, reason, "{expr}");
        }
    }

    #[test]
    fn end_of_range_is_an_error_not_a_panic() {
        let last = Date::MAX;
        for expr in ["tom", "sat", "+1d", "+1w", "+1m", "jan1"] {
            assert_eq!(
                parse_date(expr, last).unwrap_err().reason,
                OUT_OF_RANGE,
                "{expr}"
            );
        }
        // The last date is a Friday.
        assert_eq!(ok("eow", last), last);
        assert_eq!(ok("eom", last), last);
        assert_eq!(ok("dec31", last), last);
    }

    #[test]
    fn display_names_the_expression() {
        let err = parse_date("feb30", TODAY).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid date \"feb30\": february has no day 30"
        );
    }
}
