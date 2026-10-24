//! The inline capture syntax (design 2.3).

use std::collections::{HashMap, HashSet};

use jiff::civil::Date;

use crate::date::parse_date;
use crate::item::{PersonRef, Priority, Waiting};
use crate::status::Status;

const SIGILS: [char; 5] = ['+', '^', '#', '~', '>'];

/// What capture text was parsed into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureDraft {
    pub title: String,
    /// `Priority::None` without a `+N` token.
    pub priority: Priority,
    pub due_date: Option<Date>,
    /// In order of first appearance, without duplicates.
    pub labels: Vec<String>,
    /// Free text until People exist; `since` is the reference day.
    pub waiting: Option<Waiting>,
    /// `Status::Inbox` unless a `>next` / `>someday` token says otherwise.
    pub status: Status,
    /// Every sigil token in text order, applied or not.
    pub tokens: Vec<CaptureToken>,
    /// Human-readable, in text order.
    pub warnings: Vec<String>,
}

/// One sigil token of capture text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureToken {
    /// Offset of the sigil, in Unicode code points.
    pub start: usize,
    /// Exclusive end, in Unicode code points.
    pub end: usize,
    pub kind: CaptureTokenKind,
    /// False when the token failed to parse (and stayed in the title), was
    /// overridden by a later token of its kind, or the text had no title.
    pub applied: bool,
    /// Why the token was not applied; `None` when applied.
    pub warning: Option<String>,
}

/// Which field a token sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaptureTokenKind {
    /// `+1` .. `+5`
    Priority,
    /// `^<date>`
    DueDate,
    /// `#<label>`
    Label,
    /// `~<person>` or `~"<free text>"`
    Waiting,
    /// `>next` / `>someday`
    Status,
}

impl CaptureDraft {
    /// A draft of plain title text: default fields and no tokens.
    pub fn from_title(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            priority: Priority::None,
            due_date: None,
            labels: Vec::new(),
            waiting: None,
            status: Status::Inbox,
            tokens: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

/// What a successfully parsed token sets.
enum Effect {
    Priority(Priority),
    DueDate(Date),
    Label(String),
    Waiting(String),
    Status(Status),
}

enum Piece {
    Title(String),
    Token(CaptureTokenKind, Result<Effect, String>),
}

/// A word of the text, as a code-point span.
struct Word {
    start: usize,
    end: usize,
    /// A `~"…"` word closed by a quote.
    quoted: bool,
}

/// Parses capture text (design 2.3) against `today`. Never fails: a token
/// that does not parse stays in the title with a warning, and text without
/// any title words is kept whole as the title with no token applied.
pub fn parse_capture(text: &str, today: Date) -> CaptureDraft {
    let chars: Vec<char> = text.chars().collect();
    let mut draft = CaptureDraft::from_title(String::new());
    let mut title = Vec::new();
    let mut typed = Vec::new();
    let mut parsed = Vec::new();
    for word in words(&chars) {
        let raw: String = chars[word.start..word.end].iter().collect();
        let (kind, result) = match classify(&raw, word.quoted, today) {
            Piece::Title(s) => {
                title.push(s);
                continue;
            }
            Piece::Token(kind, result) => (kind, result),
        };
        let warning = match result {
            Ok(effect) => {
                parsed.push((draft.tokens.len(), effect));
                None
            }
            Err(reason) => {
                title.push(collapse_whitespace(&raw));
                Some(format!("{raw}: {reason}"))
            }
        };
        draft.tokens.push(CaptureToken {
            start: word.start,
            end: word.end,
            kind,
            applied: warning.is_none(),
            warning,
        });
        typed.push(raw);
    }

    let kept_as_typed = title.is_empty() && !draft.tokens.is_empty();
    if title.is_empty() {
        draft.title = collapse_whitespace(text);
        for (token, typed) in draft.tokens.iter_mut().zip(&typed) {
            token.applied = false;
            token.warning = Some(format!("{typed}: kept as title text"));
        }
    } else {
        draft.title = title.join(" ");
        apply_effects(&mut draft, parsed, &typed, today);
    }
    draft.warnings = draft
        .tokens
        .iter()
        .filter_map(|t| t.warning.clone())
        .collect();
    if kept_as_typed {
        draft
            .warnings
            .push("no title words: the text was kept as typed".to_owned());
    }
    draft
}

/// Applies parsed tokens (indexes into `draft.tokens`, with the text of
/// each token in `typed`). The last token of a single-valued kind wins;
/// every label applies.
fn apply_effects(
    draft: &mut CaptureDraft,
    parsed: Vec<(usize, Effect)>,
    typed: &[String],
    today: Date,
) {
    let mut winners = HashMap::new();
    let mut labels = HashSet::new();
    for (i, effect) in &parsed {
        if !matches!(effect, Effect::Label(_)) {
            winners.insert(draft.tokens[*i].kind, *i);
        }
    }
    for (i, effect) in parsed {
        if let Some(&winner) = winners.get(&draft.tokens[i].kind)
            && winner != i
        {
            let token = &mut draft.tokens[i];
            token.applied = false;
            token.warning = Some(format!("{}: overridden by {}", typed[i], typed[winner]));
            continue;
        }
        match effect {
            Effect::Priority(p) => draft.priority = p,
            Effect::DueDate(d) => draft.due_date = Some(d),
            Effect::Label(l) => {
                if labels.insert(l.clone()) {
                    draft.labels.push(l);
                }
            }
            Effect::Waiting(name) => {
                draft.waiting = Some(Waiting {
                    on: PersonRef::FreeText(name),
                    reason: String::new(),
                    since: today,
                    follow_up_on: None,
                });
            }
            Effect::Status(s) => draft.status = s,
        }
    }
}

/// Splits on whitespace, except that a word starting with `~"` runs to the
/// first later `"` followed by whitespace or the end of the text.
fn words(chars: &[char]) -> Vec<Word> {
    // Quotes that can close a `~"` word, consumed as the words advance so
    // that unterminated quotes do not rescan the rest of the text.
    let mut closes = (0..chars.len())
        .filter(|&j| chars[j] == '"' && chars.get(j + 1).is_none_or(|c| c.is_whitespace()))
        .peekable();
    let mut words = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = (start..chars.len())
            .find(|&j| chars[j].is_whitespace())
            .unwrap_or(chars.len());
        let mut quoted = false;
        if chars[start..].starts_with(&['~', '"']) {
            while closes.next_if(|&j| j < start + 2).is_some() {}
            if let Some(&close) = closes.peek() {
                end = close + 1;
                quoted = true;
            }
        }
        words.push(Word { start, end, quoted });
        i = end;
    }
    words
}

fn classify(word: &str, quoted: bool, today: Date) -> Piece {
    let mut chars = word.chars();
    let Some(sigil) = chars.next() else {
        return Piece::Title(String::new());
    };
    let rest = chars.as_str();
    if sigil == '\\' {
        let unescaped = if rest.starts_with(SIGILS) { rest } else { word };
        return Piece::Title(unescaped.to_owned());
    }
    if !SIGILS.contains(&sigil) || rest.is_empty() {
        return Piece::Title(word.to_owned());
    }
    match sigil {
        '+' => Piece::Token(CaptureTokenKind::Priority, priority(rest)),
        '^' => Piece::Token(
            CaptureTokenKind::DueDate,
            parse_date(rest, today)
                .map(Effect::DueDate)
                .map_err(|e| e.reason),
        ),
        '#' => Piece::Token(CaptureTokenKind::Label, Ok(Effect::Label(rest.to_owned()))),
        '~' => Piece::Token(CaptureTokenKind::Waiting, waiting(rest, quoted)),
        _ => Piece::Token(CaptureTokenKind::Status, status(rest)),
    }
}

fn priority(digits: &str) -> Result<Effect, String> {
    let priority = match digits {
        "1" => Priority::Highest,
        "2" => Priority::High,
        "3" => Priority::Medium,
        "4" => Priority::Low,
        "5" => Priority::Lowest,
        _ => return Err("priority must be +1 to +5".to_owned()),
    };
    Ok(Effect::Priority(priority))
}

/// `rest` follows the `~`; a quoted word still has both quotes.
fn waiting(rest: &str, quoted: bool) -> Result<Effect, String> {
    let name = if quoted {
        rest[1..rest.len() - 1].trim()
    } else if rest.starts_with('"') {
        return Err("missing closing quote".to_owned());
    } else {
        rest
    };
    if name.is_empty() {
        return Err("waiting needs a name".to_owned());
    }
    Ok(Effect::Waiting(name.to_owned()))
}

fn status(word: &str) -> Result<Effect, String> {
    if word.eq_ignore_ascii_case("next") {
        Ok(Effect::Status(Status::Next))
    } else if word.eq_ignore_ascii_case("someday") {
        Ok(Effect::Status(Status::Someday))
    } else {
        Err("status must be >next or >someday".to_owned())
    }
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    /// A Thursday.
    const TODAY: Date = date(2026, 10, 22);

    fn parse(text: &str) -> CaptureDraft {
        parse_capture(text, TODAY)
    }

    fn token(start: usize, end: usize, kind: CaptureTokenKind) -> CaptureToken {
        CaptureToken {
            start,
            end,
            kind,
            applied: true,
            warning: None,
        }
    }

    fn failed(start: usize, end: usize, kind: CaptureTokenKind, warning: &str) -> CaptureToken {
        CaptureToken {
            start,
            end,
            kind,
            applied: false,
            warning: Some(warning.to_owned()),
        }
    }

    fn free_text(name: &str) -> Option<Waiting> {
        Some(Waiting {
            on: PersonRef::FreeText(name.to_owned()),
            reason: String::new(),
            since: TODAY,
            follow_up_on: None,
        })
    }

    #[test]
    fn design_example() {
        let draft = parse("call dentist ^fri +2 #home");
        assert_eq!(
            draft,
            CaptureDraft {
                title: "call dentist".into(),
                priority: Priority::High,
                due_date: Some(date(2026, 10, 23)),
                labels: vec!["home".into()],
                waiting: None,
                status: Status::Inbox,
                tokens: vec![
                    token(13, 17, CaptureTokenKind::DueDate),
                    token(18, 20, CaptureTokenKind::Priority),
                    token(21, 26, CaptureTokenKind::Label),
                ],
                warnings: vec![],
            }
        );
    }

    #[test]
    fn each_sigil() {
        let priorities = [
            ("+1", Priority::Highest),
            ("+2", Priority::High),
            ("+3", Priority::Medium),
            ("+4", Priority::Low),
            ("+5", Priority::Lowest),
        ];
        for (sigil, want) in priorities {
            let draft = parse(&format!("x {sigil}"));
            assert_eq!(draft.priority, want, "{sigil}");
            assert_eq!(draft.title, "x");
            assert_eq!(draft.tokens, [token(2, 4, CaptureTokenKind::Priority)]);
        }

        let draft = parse("x ^tomorrow");
        assert_eq!(draft.due_date, Some(date(2026, 10, 23)));
        assert_eq!(draft.tokens, [token(2, 11, CaptureTokenKind::DueDate)]);
        assert_eq!(parse("x ^+3d").due_date, Some(date(2026, 10, 25)));
        assert_eq!(parse("x ^2026-12-01").due_date, Some(date(2026, 12, 1)));

        let draft = parse("x #Auth");
        assert_eq!(draft.labels, ["Auth"]);
        assert_eq!(draft.tokens, [token(2, 7, CaptureTokenKind::Label)]);
        assert_eq!(parse("x ##a#b").labels, ["#a#b"]);

        let draft = parse("x ~dana");
        assert_eq!(draft.waiting, free_text("dana"));
        assert_eq!(draft.tokens, [token(2, 7, CaptureTokenKind::Waiting)]);
        assert_eq!(parse("x ~dana\"").waiting, free_text("dana\""));

        for (sigil, want) in [
            (">next", Status::Next),
            (">NEXT", Status::Next),
            (">someday", Status::Someday),
            (">SomeDay", Status::Someday),
        ] {
            let draft = parse(&format!("x {sigil}"));
            assert_eq!(draft.status, want, "{sigil}");
            assert_eq!(draft.title, "x");
            assert_eq!(draft.tokens[0].kind, CaptureTokenKind::Status);
        }
    }

    #[test]
    fn plain_text_has_defaults() {
        let draft = parse("call dentist");
        assert_eq!(draft, CaptureDraft::from_title("call dentist"));
        assert_eq!(draft.priority, Priority::None);
        assert_eq!(draft.status, Status::Inbox);
        assert!(draft.tokens.is_empty() && draft.warnings.is_empty());
    }

    #[test]
    fn escapes() {
        let rows = [
            (r"\#1 fan", "#1 fan"),
            (r"\+2 \^fri \~dana \>next", "+2 ^fri ~dana >next"),
            (r"\~dana", "~dana"),
            (r"\foo", r"\foo"),
            (r"\\#x", r"\\#x"),
            (r"\", r"\"),
            (r"a\#b", r"a\#b"),
            (r#"\~"the vendor""#, r#"~"the vendor""#),
        ];
        for (text, title) in rows {
            let draft = parse(text);
            assert_eq!(draft, CaptureDraft::from_title(title), "{text}");
        }
        let draft = parse(r"\#1 fan +1");
        assert_eq!(draft.title, "#1 fan");
        assert_eq!(draft.priority, Priority::Highest);
        assert_eq!(draft.tokens, [token(8, 10, CaptureTokenKind::Priority)]);
    }

    #[test]
    fn bare_sigils_are_title_text() {
        let text = "a + b ^ c # d ~ e > f";
        assert_eq!(parse(text), CaptureDraft::from_title(text));
        assert_eq!(parse("+"), CaptureDraft::from_title("+"));
    }

    #[test]
    fn quoted_waiting() {
        let draft = parse(r#"ping ~"the vendor" re invoice"#);
        assert_eq!(draft.title, "ping re invoice");
        assert_eq!(draft.waiting, free_text("the vendor"));
        assert_eq!(draft.tokens, [token(5, 18, CaptureTokenKind::Waiting)]);

        let draft = parse(r#"x ~"  spaced   out " y"#);
        assert_eq!(draft.waiting, free_text("spaced   out"));
        assert_eq!(draft.title, "x y");

        // The closing quote is the first one followed by whitespace or the end.
        let draft = parse(r#"~"a"b c" d"#);
        assert_eq!(draft.waiting, free_text(r#"a"b c"#));
        assert_eq!(draft.title, "d");
        assert_eq!(draft.tokens, [token(0, 8, CaptureTokenKind::Waiting)]);

        let draft = parse(r#"x ~"dana""#);
        assert_eq!(draft.waiting, free_text("dana"));
        assert_eq!(draft.tokens, [token(2, 9, CaptureTokenKind::Waiting)]);
    }

    #[test]
    fn quoted_waiting_failures() {
        let draft = parse(r#"ask ~"the vendor"#);
        assert_eq!(draft.title, r#"ask ~"the vendor"#);
        assert_eq!(draft.waiting, None);
        assert_eq!(
            draft.tokens,
            [failed(
                4,
                9,
                CaptureTokenKind::Waiting,
                r#"~"the: missing closing quote"#
            )]
        );
        assert_eq!(draft.warnings, [r#"~"the: missing closing quote"#]);

        let draft = parse(r#"ask ~"the vendor"x"#);
        assert_eq!(draft.title, r#"ask ~"the vendor"x"#);
        assert_eq!(draft.tokens[0].end, 9);
        assert!(!draft.tokens[0].applied);

        let draft = parse(r#"x ~" y"#);
        assert_eq!(draft.title, r#"x ~" y"#);
        assert_eq!(draft.warnings, [r#"~": missing closing quote"#]);

        let draft = parse(r#"x ~"""#);
        assert_eq!(draft.title, r#"x ~"""#);
        assert_eq!(
            draft.tokens,
            [failed(
                2,
                5,
                CaptureTokenKind::Waiting,
                r#"~"": waiting needs a name"#
            )]
        );

        let draft = parse(r#"x ~"   " y"#);
        assert_eq!(draft.title, r#"x ~" " y"#);
        assert_eq!(draft.warnings, [r#"~"   ": waiting needs a name"#]);
        assert_eq!(draft.tokens[0].end, 8);
    }

    #[test]
    fn later_tokens_override_earlier_ones() {
        let draft = parse("x +1 +2");
        assert_eq!(draft.priority, Priority::High);
        assert_eq!(draft.title, "x");
        assert_eq!(
            draft.tokens,
            [
                failed(2, 4, CaptureTokenKind::Priority, "+1: overridden by +2"),
                token(5, 7, CaptureTokenKind::Priority),
            ]
        );
        assert_eq!(draft.warnings, ["+1: overridden by +2"]);

        let draft = parse("x +1 +2 +3");
        assert_eq!(draft.priority, Priority::Medium);
        assert_eq!(
            draft.warnings,
            ["+1: overridden by +3", "+2: overridden by +3"]
        );

        let draft = parse("x ^tom ^mon");
        assert_eq!(draft.due_date, Some(date(2026, 10, 26)));
        assert_eq!(draft.warnings, ["^tom: overridden by ^mon"]);

        let draft = parse(r#"x ~a ~"b c""#);
        assert_eq!(draft.waiting, free_text("b c"));
        assert_eq!(draft.warnings, [r#"~a: overridden by ~"b c""#]);

        let draft = parse("x >next >someday");
        assert_eq!(draft.status, Status::Someday);
        assert_eq!(draft.warnings, [">next: overridden by >someday"]);

        // A failed token overrides nothing.
        let draft = parse("x +1 +9");
        assert_eq!(draft.priority, Priority::Highest);
        assert_eq!(draft.title, "x +9");
        assert_eq!(draft.warnings, ["+9: priority must be +1 to +5"]);

        // Kinds do not override each other.
        let draft = parse("x +1 ^tom #a ~b >next");
        assert!(draft.tokens.iter().all(|t| t.applied));
        assert!(draft.warnings.is_empty());
    }

    #[test]
    fn duplicate_labels_are_stored_once() {
        let draft = parse("x #a #b #a #A");
        assert_eq!(draft.labels, ["a", "b", "A"]);
        assert_eq!(draft.tokens.len(), 4);
        assert!(draft.tokens.iter().all(|t| t.applied));
        assert!(draft.warnings.is_empty());
    }

    #[test]
    fn failed_tokens_stay_in_the_title() {
        let rows = [
            (
                "fix +7 thing",
                "fix +7 thing",
                "+7: priority must be +1 to +5",
            ),
            ("x +0", "x +0", "+0: priority must be +1 to +5"),
            ("x +6", "x +6", "+6: priority must be +1 to +5"),
            ("x +12", "x +12", "+12: priority must be +1 to +5"),
            ("x +x", "x +x", "+x: priority must be +1 to +5"),
            ("x +1x", "x +1x", "+1x: priority must be +1 to +5"),
            ("x ++1", "x ++1", "++1: priority must be +1 to +5"),
            ("x ^someday", "x ^someday", "^someday: not a date"),
            ("x ^feb30", "x ^feb30", "^feb30: february has no day 30"),
            ("x ^^fri", "x ^^fri", "^^fri: not a date"),
            (
                "x ^0000-03-01",
                "x ^0000-03-01",
                "^0000-03-01: the year must be 1 to 9999",
            ),
            (
                "x >later",
                "x >later",
                ">later: status must be >next or >someday",
            ),
            (
                "x >inbox",
                "x >inbox",
                ">inbox: status must be >next or >someday",
            ),
        ];
        for (text, title, warning) in rows {
            let draft = parse(text);
            assert_eq!(draft.title, title, "{text}");
            assert_eq!(draft.warnings, [warning], "{text}");
            assert_eq!(draft.tokens.len(), 1, "{text}");
            assert!(!draft.tokens[0].applied, "{text}");
            assert_eq!(draft.tokens[0].warning.as_deref(), Some(warning));
            assert_eq!(draft.priority, Priority::None);
            assert_eq!(draft.due_date, None);
            assert_eq!(draft.status, Status::Inbox);
        }
        // A failed token is a title word, so the text still has a title.
        let draft = parse("+9 +1");
        assert_eq!(draft.title, "+9");
        assert_eq!(draft.priority, Priority::Highest);
    }

    #[test]
    fn text_without_title_words_is_kept_as_typed() {
        let draft = parse("+1 ^fri");
        assert_eq!(draft.title, "+1 ^fri");
        assert_eq!(draft.priority, Priority::None);
        assert_eq!(draft.due_date, None);
        assert_eq!(
            draft.tokens,
            [
                failed(0, 2, CaptureTokenKind::Priority, "+1: kept as title text"),
                failed(3, 7, CaptureTokenKind::DueDate, "^fri: kept as title text"),
            ]
        );
        assert_eq!(
            draft.warnings,
            [
                "+1: kept as title text",
                "^fri: kept as title text",
                "no title words: the text was kept as typed",
            ]
        );

        let draft = parse("  #a \t >next  +1 +2 ");
        assert_eq!(draft.title, "#a >next +1 +2");
        assert!(draft.labels.is_empty());
        assert_eq!(draft.status, Status::Inbox);
        assert_eq!(draft.priority, Priority::None);
        assert!(draft.tokens.iter().all(|t| !t.applied));

        let draft = parse(r#"~"the vendor""#);
        assert_eq!(draft.title, r#"~"the vendor""#);
        assert_eq!(draft.waiting, None);
    }

    #[test]
    fn empty_text() {
        for text in ["", "   ", " \t\n "] {
            assert_eq!(parse(text), CaptureDraft::from_title(""), "{text:?}");
        }
    }

    #[test]
    fn title_whitespace_is_collapsed() {
        assert_eq!(parse("  call \t dentist\n").title, "call dentist");
        assert_eq!(parse("a\u{3000}b").title, "a b");
    }

    #[test]
    fn offsets_are_code_points() {
        let draft = parse("café ^tom");
        assert_eq!(draft.title, "café");
        assert_eq!(draft.tokens, [token(5, 9, CaptureTokenKind::DueDate)]);

        let draft = parse("日本語 #タグ 🎉 +1");
        assert_eq!(draft.title, "日本語 🎉");
        assert_eq!(draft.labels, ["タグ"]);
        assert_eq!(
            draft.tokens,
            [
                token(4, 7, CaptureTokenKind::Label),
                token(10, 12, CaptureTokenKind::Priority),
            ]
        );

        let draft = parse(r#"ñ ~"José Ñ" ok"#);
        assert_eq!(draft.waiting, free_text("José Ñ"));
        assert_eq!(draft.tokens, [token(2, 11, CaptureTokenKind::Waiting)]);
    }

    #[test]
    fn warnings_are_in_text_order() {
        let draft = parse("+9 x +1 +2 ^nope");
        assert_eq!(draft.title, "+9 x ^nope");
        assert_eq!(draft.priority, Priority::High);
        assert_eq!(
            draft.warnings,
            [
                "+9: priority must be +1 to +5",
                "+1: overridden by +2",
                "^nope: not a date",
            ]
        );
    }

    #[test]
    fn hostile_text_parses_in_linear_time() {
        let started = std::time::Instant::now();
        // Every `~"` word looks for a closing quote that never comes.
        let draft = parse(&"~\"a ".repeat(40_000));
        assert_eq!(draft.tokens.len(), 40_000);
        assert!(draft.tokens.iter().all(|t| !t.applied));
        let labels: String = (0..40_000).map(|i| format!(" #{i}")).collect();
        let draft = parse(&format!("x{labels}"));
        assert_eq!(draft.labels.len(), 40_000);
        assert_eq!(draft.labels[39_999], "39999");
        let elapsed = started.elapsed();
        assert!(elapsed < std::time::Duration::from_secs(10), "{elapsed:?}");
    }

    #[test]
    fn invariants_hold_for_assorted_text() {
        let texts = [
            "call dentist ^fri +2 #home",
            "+1 +2",
            r#"~"a b" ~"c"#,
            r"\+1 + ++ ^ ^^ # ## ~ ~~ > >>",
            "  ",
            "x\u{a0}+1\u{2003}#é",
            r#"~"" ~"x"y" z"#,
        ];
        for text in texts {
            let draft = parse(text);
            let len = text.chars().count();
            assert_eq!(draft.title.is_empty(), text.trim().is_empty(), "{text}");
            let mut last_end = 0;
            for t in &draft.tokens {
                assert!(
                    last_end <= t.start && t.start < t.end && t.end <= len,
                    "{text}"
                );
                assert_eq!(t.applied, t.warning.is_none(), "{text}");
                last_end = t.end;
            }
            let token_warnings: Vec<_> = draft
                .tokens
                .iter()
                .filter_map(|t| t.warning.clone())
                .collect();
            assert!(draft.warnings.starts_with(&token_warnings), "{text}");
        }
    }
}
