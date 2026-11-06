//! Query text (design 4.8 "Query text"): a view's filter, sort and count
//! flag written as words, as `:view new` and `:view edit` take them.
//!
//! ```text
//! is:next,in_progress #work priority>=high due_date<=today+3d sort:-priority,due_date
//! any:(due_date<=today is:in_progress) include:done count:off
//! ```
//!
//! Terms are separated by whitespace and ANDed. [`parse_query`] lowers the
//! text to a [`Filter`] and sort, collecting every error with its span;
//! [`print_query`] writes a filter and sort back as canonical text, which
//! parses to the same filter and sort. Field names are [`ItemField::path`]s.
//!
//! - `field:a,b` is IN (one value or more), and on a set CONTAINS_ANY;
//!   `field:=v` is EQ; `field!=v`, `<`, `<=`, `>`, `>=` as written;
//!   `field:all=a,b` CONTAINS_ALL; `field~words` MATCHES.
//! - `is:s,…` is `status:s,…`; `has:f` and `no:f` are IS_SET and IS_UNSET;
//!   `#label` is `labels:label`; a bare word or `"quoted text"` is
//!   `title~…` (adjacent bare words join into one MATCHES).
//! - `-` negates: `-f:v` is `f!=v`, `-has:f` is `no:f`, `-no:f` is
//!   `has:f`. A filter has no NOT, so nothing else negates.
//! - `any:( … )` is a group whose terms are alternatives; `( … )` one whose
//!   terms all hold. The first `any:(…)` at the top fills the filter's own
//!   `any_of`.
//! - `sort:k,-k` sorts (`-` descending); `include:archived,done,deferred,
//!   deleted` lifts the implicit rules; `count:off` hides the view's count
//!   (`count:on`, the default, shows it).
//! - Values: enum names in snake_case (`in_progress`, `high`, `auto_done`);
//!   dates `2026-11-02`, `today`, `week_start`, `week_end`, each with an
//!   optional `+3d` / `-2w`; `this_week` and `2026-W45` for `focus_week`;
//!   numbers; durations (`2h30m`, `-1h`); `true` / `false`; timestamps
//!   (`2026-11-02T10:00:00Z`); ids; `person:<id>` for a Person; text,
//!   `"quoted"` when it has spaces, commas, parentheses, quotes or an
//!   operator character (`\"` and `\\` escape).

use std::fmt::Write as _;

use jiff::civil::Date;
use jiff::{SignedDuration, Timestamp};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::field::ItemField;
use crate::filter::{
    Anchor, Clause, Filter, FilterError, Group, MAX_GROUP_DEPTH, Operand, Operator, Predicate,
    RelativeDate, SortKey, ValueClass, target, validate_sort,
};
use crate::item::PersonRef;
use crate::mutation::is_iso_week;
use crate::status::Status;
use crate::value::FieldValue;

/// The longest query text, in bytes of UTF-8.
pub const MAX_QUERY_BYTES: usize = 4096;

/// What a Person record's id is written after (`assignee:=person:01J…`);
/// any other person value is free text.
const PERSON_PREFIX: &str = "person:";

/// A query's filter, sort and count flag.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub filter: Filter,
    pub sort: Vec<SortKey>,
    /// False with `count:off`.
    pub show_count: bool,
}

impl Default for Query {
    fn default() -> Self {
        Query {
            filter: Filter::default(),
            sort: Vec::new(),
            show_count: true,
        }
    }
}

/// What [`parse_query`] made of query text.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedQuery {
    pub query: Query,
    /// In text order; the query is usable only when there are none.
    pub errors: Vec<QueryError>,
}

/// A problem with a span of query text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryError {
    /// Offset of the first character, in Unicode code points.
    pub start: usize,
    /// Exclusive end, in Unicode code points.
    pub end: usize,
    pub message: String,
}

// ---- lexing ----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    /// A word: everything up to whitespace or a parenthesis, with quoted
    /// stretches kept whole.
    Word(String),
    Open,
    Close,
}

/// A token and its span in code points.
#[derive(Debug, Clone)]
struct Lexed {
    tok: Tok,
    start: usize,
    end: usize,
    /// Whether the word is one quoted string, `"…"`.
    quoted: bool,
}

/// Splits text into words and parentheses. A `"` starts a quoted stretch
/// that runs to the next unescaped `"`, spaces and parentheses included;
/// inside it, `\"` and `\\` are escapes. `any:(` is a word ending in `(`
/// followed by an Open.
fn lex(text: &str, errors: &mut Vec<QueryError>) -> Vec<Lexed> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '(' || c == ')' {
            out.push(Lexed {
                tok: if c == '(' { Tok::Open } else { Tok::Close },
                start: i,
                end: i + 1,
                quoted: false,
            });
            i += 1;
            continue;
        }
        let start = i;
        let mut word = String::new();
        let mut quoted_whole = c == '"';
        while i < chars.len() {
            let c = chars[i];
            if c.is_whitespace() || c == ')' {
                break;
            }
            if c == '(' {
                // `any:(`: the word ends here, and the parenthesis follows.
                break;
            }
            if c == '"' {
                let open = i;
                i += 1;
                let mut closed = false;
                // Keep the quotes, so the term parser can tell a quoted
                // value from a bare one; it unquotes.
                word.push('"');
                while i < chars.len() {
                    match chars[i] {
                        '\\' if i + 1 < chars.len() && matches!(chars[i + 1], '"' | '\\') => {
                            word.push('\\');
                            word.push(chars[i + 1]);
                            i += 2;
                        }
                        '"' => {
                            word.push('"');
                            i += 1;
                            closed = true;
                            break;
                        }
                        ch => {
                            word.push(ch);
                            i += 1;
                        }
                    }
                }
                if !closed {
                    errors.push(QueryError {
                        start: open,
                        end: chars.len(),
                        message: "a quote is not closed".to_owned(),
                    });
                }
                continue;
            }
            quoted_whole = false;
            word.push(c);
            i += 1;
        }
        let quoted =
            quoted_whole && word.len() >= 2 && word.starts_with('"') && word.ends_with('"');
        out.push(Lexed {
            tok: Tok::Word(word),
            start,
            end: i,
            quoted,
        });
    }
    out
}

/// The text of a word with its quoted stretches unquoted.
fn unquote(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    let mut chars = word.chars().peekable();
    let mut inside = false;
    while let Some(c) = chars.next() {
        match c {
            '"' => inside = !inside,
            '\\' if inside && matches!(chars.peek(), Some('"' | '\\')) => {
                out.push(chars.next().expect("peeked"));
            }
            c => out.push(c),
        }
    }
    out
}

/// Splits `s` at the commas outside quotes.
fn split_commas(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut inside = false;
    let mut escaped = false;
    let mut from = 0;
    for (i, c) in s.char_indices() {
        match c {
            '\\' if inside && !escaped => {
                escaped = true;
                continue;
            }
            '"' if !escaped => inside = !inside,
            ',' if !inside => {
                parts.push(&s[from..i]);
                from = i + 1;
            }
            _ => {}
        }
        escaped = false;
    }
    parts.push(&s[from..]);
    parts
}

// ---- parsing ---------------------------------------------------------------

/// A clause with the span of the term it came from, and the spans of its
/// own clauses when it is a group, so a validator path finds its term.
#[derive(Debug, Clone)]
struct Spanned {
    clause: Clause,
    start: usize,
    end: usize,
    /// For a group: its `all_of` and `any_of` children.
    children: Vec<Spanned>,
    any: bool,
}

struct Parser<'a> {
    toks: &'a [Lexed],
    at: usize,
    errors: Vec<QueryError>,
    sort: Option<(Vec<SortKey>, usize, usize)>,
    include: [bool; 4],
    show_count: bool,
}

/// Parses query text. Parsing goes on after an error, so every one is
/// reported; then the filter and sort are validated, an error naming the
/// span of the term it is about. Text over [`MAX_QUERY_BYTES`] is one error.
pub fn parse_query(text: &str) -> ParsedQuery {
    let len = text.chars().count();
    if text.len() > MAX_QUERY_BYTES {
        return ParsedQuery {
            query: Query::default(),
            errors: vec![QueryError {
                start: 0,
                end: len,
                message: format!("is longer than {MAX_QUERY_BYTES} bytes"),
            }],
        };
    }
    let mut lex_errors = Vec::new();
    let toks = lex(text, &mut lex_errors);
    let mut p = Parser {
        toks: &toks,
        at: 0,
        errors: lex_errors,
        sort: None,
        include: [false; 4],
        show_count: true,
    };
    let terms = p.terms(false, 0);

    // The first top-level any:( … ) is the filter's any_of.
    let mut all_of = Vec::new();
    let mut any_of = Vec::new();
    for term in terms {
        let top_any = term.any
            && any_of.is_empty()
            && matches!(&term.clause, Clause::Group(g) if g.all_of.is_empty());
        if top_any {
            any_of = term.children;
        } else {
            all_of.push(term);
        }
    }
    let [archived, done, deferred, deleted] = p.include;
    let filter = Filter {
        all_of: all_of.iter().map(|t| t.clause.clone()).collect(),
        any_of: any_of.iter().map(|t| t.clause.clone()).collect(),
        include_archived: archived,
        include_done: done,
        include_deferred: deferred,
        include_deleted: deleted,
    };
    let (sort, sort_start, sort_end) = p.sort.take().unwrap_or((Vec::new(), 0, 0));
    let mut errors = p.errors;
    if errors.is_empty() {
        if let Err(e) = filter.validate() {
            let (start, end) = span_of(&e, &all_of, &any_of).unwrap_or((0, len));
            errors.push(QueryError {
                start,
                end,
                message: e.reason,
            });
        } else if let Err(e) = validate_sort(&sort) {
            errors.push(QueryError {
                start: sort_start,
                end: sort_end,
                message: e.reason,
            });
        }
    }
    errors.sort_by_key(|e| (e.start, e.end));
    ParsedQuery {
        query: Query {
            filter,
            sort,
            show_count: p.show_count,
        },
        errors,
    }
}

/// The span of the term a validator error's path is about: the deepest
/// clause the path names.
fn span_of(e: &FilterError, all_of: &[Spanned], any_of: &[Spanned]) -> Option<(usize, usize)> {
    let mut lists = (all_of, any_of);
    let mut found = None;
    for part in e.path.split('.') {
        let pick = |name: &str, list: &[Spanned]| -> Option<usize> {
            part.strip_prefix(name)?
                .strip_prefix('[')?
                .strip_suffix(']')?
                .parse::<usize>()
                .ok()
                .filter(|&i| i < list.len())
        };
        let node = if let Some(i) = pick("all_of", lists.0) {
            &lists.0[i]
        } else if let Some(i) = pick("any_of", lists.1) {
            &lists.1[i]
        } else {
            continue;
        };
        found = Some((node.start, node.end));
        // An any:( … ) group's terms are its any_of, a ( … ) group's its all_of.
        lists = if node.any {
            (&[][..], &node.children[..])
        } else {
            (&node.children[..], &[][..])
        };
    }
    found
}

impl Parser<'_> {
    fn error(&mut self, start: usize, end: usize, message: impl Into<String>) {
        self.errors.push(QueryError {
            start,
            end,
            message: message.into(),
        });
    }

    /// Terms until a `)` (when `nested`) or the end.
    fn terms(&mut self, nested: bool, depth: usize) -> Vec<Spanned> {
        let mut out: Vec<Spanned> = Vec::new();
        // Adjacent bare words, joined into one title MATCHES.
        let mut words: Option<(String, usize, usize)> = None;
        let flush = |words: &mut Option<(String, usize, usize)>, out: &mut Vec<Spanned>| {
            if let Some((text, start, end)) = words.take() {
                out.push(leaf(
                    Predicate::new(
                        ItemField::Title,
                        Operator::Matches,
                        vec![Operand::Text(text)],
                    ),
                    start,
                    end,
                ));
            }
        };
        while let Some(t) = self.toks.get(self.at).cloned() {
            match &t.tok {
                Tok::Close if nested => break,
                Tok::Close => {
                    // Reported, and skipped: the terms after it still count.
                    flush(&mut words, &mut out);
                    self.at += 1;
                    self.error(t.start, t.end, "a ) without its (");
                }
                Tok::Open => {
                    flush(&mut words, &mut out);
                    self.at += 1;
                    if let Some(group) = self.group(t.start, false, depth) {
                        out.push(group);
                    }
                }
                Tok::Word(w) => {
                    self.at += 1;
                    if let Some(text) = self.bare_text(w, &t) {
                        words = Some(match words.take() {
                            Some((mut joined, start, _)) => {
                                joined.push(' ');
                                joined.push_str(&text);
                                (joined, start, t.end)
                            }
                            None => (text, t.start, t.end),
                        });
                        continue;
                    }
                    flush(&mut words, &mut out);
                    if let Some(term) = self.term(w, &t, depth) {
                        out.push(term);
                    }
                }
            }
        }
        flush(&mut words, &mut out);
        out
    }

    /// The words of a bare word or `"quoted text"` that is free title text;
    /// `None` for any other term.
    fn bare_text(&self, w: &str, t: &Lexed) -> Option<String> {
        // A word that opens a quote is text, closed or not (the lexer has
        // reported a quote left open).
        if t.quoted || w.starts_with('"') {
            return Some(unquote(w));
        }
        let first = w.chars().next()?;
        if matches!(first, '#' | '-') || w.contains([':', '<', '>', '=', '~', '!', '"']) {
            return None;
        }
        // `any:` with its parenthesis apart is not text either.
        Some(w.to_owned())
    }

    /// The group whose `(` was at `start`, `depth` groups deep around it;
    /// the `)` is consumed. One nested too deep is reported and skipped
    /// without descending, so no text can recurse further than the limit.
    fn group(&mut self, start: usize, any: bool, depth: usize) -> Option<Spanned> {
        if depth >= MAX_GROUP_DEPTH {
            let mut open = 1;
            let mut end = start + 1;
            while let Some(t) = self.toks.get(self.at) {
                self.at += 1;
                end = t.end;
                match t.tok {
                    Tok::Open => open += 1,
                    Tok::Close => open -= 1,
                    Tok::Word(_) => {}
                }
                if open == 0 {
                    break;
                }
            }
            let message = if open == 0 {
                format!("groups nest at most {MAX_GROUP_DEPTH} deep")
            } else {
                "a ( is not closed".to_owned()
            };
            self.error(start, end, message);
            return None;
        }
        let children = self.terms(true, depth + 1);
        let end = match self.toks.get(self.at) {
            Some(Lexed {
                tok: Tok::Close,
                end,
                ..
            }) => {
                let end = *end;
                self.at += 1;
                end
            }
            _ => {
                let end = self.toks.last().map_or(start + 1, |t| t.end);
                self.error(start, end, "a ( is not closed");
                return None;
            }
        };
        let clauses: Vec<Clause> = children.iter().map(|c| c.clause.clone()).collect();
        let group = if any {
            Group {
                all_of: Vec::new(),
                any_of: clauses,
            }
        } else {
            Group {
                all_of: clauses,
                any_of: Vec::new(),
            }
        };
        Some(Spanned {
            clause: Clause::Group(group),
            start,
            end,
            children,
            any,
        })
    }

    /// One term that is not free text.
    fn term(&mut self, w: &str, t: &Lexed, depth: usize) -> Option<Spanned> {
        let (start, end) = (t.start, t.end);
        let lower = w.to_ascii_lowercase();
        // any:( … ): the word is `any:` and an Open follows.
        if lower == "any:" {
            if let Some(Lexed { tok: Tok::Open, .. }) = self.toks.get(self.at) {
                self.at += 1;
                let mut group = self.group(start, true, depth)?;
                group.start = start;
                return Some(group);
            }
            self.error(
                start,
                end,
                "any: takes ( … ), e.g. any:(is:next has:waiting)",
            );
            return None;
        }
        if lower == "-any:" {
            self.error(start, end, "a group cannot be negated");
            return None;
        }
        if let Some(rest) = strip_ci(w, "sort:") {
            self.sort_term(rest, start, end);
            return None;
        }
        if let Some(rest) = strip_ci(w, "include:") {
            self.include_term(rest, start, end);
            return None;
        }
        if let Some(rest) = strip_ci(w, "count:") {
            match rest.to_ascii_lowercase().as_str() {
                "on" => self.show_count = true,
                "off" => self.show_count = false,
                _ => self.error(start, end, "count: takes on or off"),
            }
            return None;
        }
        let (negated, body) = match w.strip_prefix('-') {
            Some(body) => (true, body),
            None => (false, w),
        };
        let fail = |p: &mut Self, message: String| {
            p.error(start, end, message);
            None
        };
        if let Some(label) = body.strip_prefix('#') {
            if negated {
                return fail(
                    self,
                    "a label cannot be negated: a filter has no NOT".to_owned(),
                );
            }
            let label = unquote(label);
            if label.is_empty() {
                return fail(self, "a label after #".to_owned());
            }
            return Some(leaf(
                Predicate::new(
                    ItemField::Labels,
                    Operator::ContainsAny,
                    vec![Operand::Value(FieldValue::String(label))],
                ),
                start,
                end,
            ));
        }
        for (prefix, set) in [("has:", true), ("no:", false)] {
            if let Some(name) = strip_ci(body, prefix) {
                let Some(field) = field_named(name) else {
                    return fail(self, unknown_field(name));
                };
                let op = if set != negated {
                    Operator::IsSet
                } else {
                    Operator::IsUnset
                };
                return Some(leaf(Predicate::new(field, op, Vec::new()), start, end));
            }
        }
        if let Some(statuses) = strip_ci(body, "is:") {
            return self.comparison(ItemField::Status, ":", statuses, negated, start, end);
        }
        // field op values
        let Some((name, op, values)) = split_comparison(body) else {
            return fail(
                self,
                format!(
                    "{body:?} is not a term: use field:value, has:field, is:status, #label or words"
                ),
            );
        };
        let Some(field) = field_named(name) else {
            return fail(self, unknown_field(name));
        };
        self.comparison(field, op, values, negated, start, end)
    }

    fn comparison(
        &mut self,
        field: ItemField,
        op: &str,
        values: &str,
        negated: bool,
        start: usize,
        end: usize,
    ) -> Option<Spanned> {
        let fail = |p: &mut Self, message: String| {
            p.error(start, end, message);
            None
        };
        let class = target(field).class;
        let parts = split_commas(values);
        let op = match (op, negated) {
            (":", false) if class == ValueClass::Set => Operator::ContainsAny,
            (":", false) => Operator::In,
            (":=", false) => Operator::Eq,
            (":", true) | (":=", true) if parts.len() == 1 && class != ValueClass::Set => {
                Operator::Ne
            }
            (":all=", false) => Operator::ContainsAll,
            ("!=", false) => Operator::Ne,
            ("<", false) => Operator::Lt,
            ("<=", false) => Operator::Le,
            (">", false) => Operator::Gt,
            (">=", false) => Operator::Ge,
            ("~", false) => Operator::Matches,
            (":", true) if class == ValueClass::Set => {
                return fail(
                    self,
                    format!(
                        "{} is a set, which cannot be negated: a filter has no NOT",
                        field.path()
                    ),
                );
            }
            (":", true) => {
                return fail(
                    self,
                    "only one value can be negated (-f:v is f!=v): a filter has no NOT".to_owned(),
                );
            }
            _ => {
                return fail(
                    self,
                    "only field:value and has: can be negated (-f:v is f!=v): a filter has no NOT"
                        .to_owned(),
                );
            }
        };
        if op == Operator::Matches {
            let text = unquote(values);
            return Some(leaf(
                Predicate::new(field, op, vec![Operand::Text(text)]),
                start,
                end,
            ));
        }
        if values.is_empty() {
            return fail(self, "a value after the operator".to_owned());
        }
        let mut operands = Vec::with_capacity(parts.len());
        for part in parts {
            match operand(field, &unquote(part), part.starts_with('"')) {
                Ok(o) => operands.push(o),
                Err(why) => return fail(self, why),
            }
        }
        Some(leaf(Predicate::new(field, op, operands), start, end))
    }

    fn sort_term(&mut self, rest: &str, start: usize, end: usize) {
        if self.sort.is_some() {
            self.error(start, end, "one sort: per query; list its keys with commas");
            return;
        }
        let mut keys = Vec::new();
        for part in rest.split(',') {
            let (descending, name) = match part.strip_prefix('-') {
                Some(name) => (true, name),
                None => (false, part),
            };
            match field_named(name) {
                Some(key) => keys.push(SortKey { key, descending }),
                None => {
                    self.error(start, end, unknown_field(name));
                    return;
                }
            }
        }
        self.sort = Some((keys, start, end));
    }

    fn include_term(&mut self, rest: &str, start: usize, end: usize) {
        for part in rest.split(',') {
            let i = match part.to_ascii_lowercase().as_str() {
                "archived" => 0,
                "done" => 1,
                "deferred" => 2,
                "deleted" => 3,
                _ => {
                    self.error(
                        start,
                        end,
                        format!(
                            "include: takes archived, done, deferred and deleted, not {part:?}"
                        ),
                    );
                    return;
                }
            };
            self.include[i] = true;
        }
    }
}

fn leaf(p: Predicate, start: usize, end: usize) -> Spanned {
    Spanned {
        clause: Clause::Predicate(p),
        start,
        end,
        children: Vec::new(),
        any: false,
    }
}

/// `s` without `prefix`, matched in any case.
fn strip_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// The field named `name`, in any case.
fn field_named(name: &str) -> Option<ItemField> {
    ItemField::from_path(&name.to_ascii_lowercase())
}

fn unknown_field(name: &str) -> String {
    if name.is_empty() {
        "a field name, e.g. due_date".to_owned()
    } else {
        format!("unknown field {name:?}")
    }
}

/// `field`, the operator and the values of `field op values`. The field
/// name runs to the first operator character outside quotes.
fn split_comparison(s: &str) -> Option<(&str, &'static str, &str)> {
    let at = s.find([':', '<', '>', '=', '!', '~'])?;
    let (name, rest) = s.split_at(at);
    const OPS: [&str; 9] = [":all=", ":=", "!=", "<=", ">=", ":", "<", ">", "~"];
    let op = OPS.iter().find(|op| rest.starts_with(**op))?;
    Some((name, op, &rest[op.len()..]))
}

/// One value of a comparison on `field`, from its text (unquoted already).
fn operand(field: ItemField, text: &str, quoted: bool) -> Result<Operand, String> {
    use ValueClass as C;
    let class = target(field).class;
    let value = |v: FieldValue| Ok(Operand::Value(v));
    match class {
        C::Enum | C::Priority | C::Energy => enum_value(field, text).map(Operand::Value),
        C::String | C::Set => value(FieldValue::String(text.to_owned())),
        C::Markdown => value(FieldValue::Markdown(text.to_owned())),
        C::Id => text
            .parse()
            .map(FieldValue::ItemId)
            .map(Operand::Value)
            .map_err(|_| format!("{text:?} is not an id")),
        C::Date if !quoted => date_operand(text),
        C::Date => Err(format!("{text:?} is not a date")),
        C::FocusWeek => {
            if text.eq_ignore_ascii_case("this_week") {
                Ok(Operand::RelativeDate(RelativeDate::new(
                    Anchor::CurrentWeek,
                    0,
                )))
            } else if is_iso_week(text) {
                value(FieldValue::IsoWeek(text.to_owned()))
            } else {
                Err(format!("{text:?} is not a week: use this_week or YYYY-Www"))
            }
        }
        C::Instant => text
            .parse::<Timestamp>()
            .map(FieldValue::Timestamp)
            .map(Operand::Value)
            .map_err(|_| format!("{text:?} is not a time, e.g. 2026-11-02T10:00:00Z")),
        C::Number => match text.parse::<f64>() {
            Ok(n) if n.is_finite() => value(FieldValue::Number(n)),
            _ => Err(format!("{text:?} is not a number")),
        },
        C::Uint => text
            .parse()
            .map(FieldValue::Uint)
            .map(Operand::Value)
            .map_err(|_| format!("{text:?} is not a whole number")),
        C::Duration => text
            .parse::<SignedDuration>()
            .map(FieldValue::Duration)
            .map(Operand::Value)
            .map_err(|_| format!("{text:?} is not a duration, e.g. 2h30m")),
        C::Bool => match text.to_ascii_lowercase().as_str() {
            "true" => value(FieldValue::Bool(true)),
            "false" => value(FieldValue::Bool(false)),
            _ => Err("takes true or false".to_owned()),
        },
        C::Person => match text.strip_prefix(PERSON_PREFIX) {
            Some(id) if !quoted => id
                .parse()
                .map(|id| FieldValue::Person(PersonRef::Person(id)))
                .map(Operand::Value)
                .map_err(|_| format!("{id:?} is not a person id")),
            _ => value(FieldValue::Person(PersonRef::FreeText(text.to_owned()))),
        },
        C::Presence | C::PendingFields | C::RemoteExtras => {
            Err(format!("{} takes no values: use has: or no:", field.path()))
        }
    }
}

/// A date operand: ISO, or an anchor with an optional `+Nd` / `-Nw`.
fn date_operand(text: &str) -> Result<Operand, String> {
    if let Ok(d) = text.parse::<Date>() {
        return Ok(Operand::Value(FieldValue::Date(d)));
    }
    let lower = text.to_ascii_lowercase();
    let at = lower.find(['+', '-']).unwrap_or(lower.len());
    let (name, offset) = lower.split_at(at);
    let anchor = match name {
        "today" => Anchor::Today,
        "week_start" => Anchor::StartOfWeek,
        "week_end" => Anchor::EndOfWeek,
        _ => {
            return Err(format!(
                "{text:?} is not a date: use YYYY-MM-DD, today, week_start or week_end, with +3d or -2w"
            ));
        }
    };
    let offset_days = if offset.is_empty() {
        0
    } else {
        parse_offset(offset)
            .ok_or_else(|| format!("{offset:?} is not an offset: use +3d or -2w"))?
    };
    Ok(Operand::RelativeDate(RelativeDate::new(
        anchor,
        offset_days,
    )))
}

/// `+3d`, `-2w`: days, a week being 7.
fn parse_offset(s: &str) -> Option<i32> {
    let (sign, rest) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    let (n, unit) = rest.split_at(rest.find(|c: char| !c.is_ascii_digit())?);
    let n: i64 = n.parse().ok()?;
    let days = match unit {
        "d" => n,
        "w" => n.checked_mul(7)?,
        _ => return None,
    };
    i32::try_from(days * sign).ok()
}

/// The enum value named `name` for `field`, by its serde name.
fn enum_value(field: ItemField, name: &str) -> Result<FieldValue, String> {
    use crate::item::{ArchiveReason, CreatedVia, Energy, LinkState, Priority};
    use crate::status::{ResolutionKind, StatusCategory, TodoRefinement};
    use ItemField as F;
    let name = name.to_ascii_lowercase();
    fn named<T: DeserializeOwned>(name: &str) -> Option<T> {
        serde_json::from_value(serde_json::Value::String(name.to_owned())).ok()
    }
    let value = match field {
        F::Status => named::<Status>(&name).map(FieldValue::Status),
        F::StatusCategory => named::<StatusCategory>(&name).map(FieldValue::StatusCategory),
        F::ResolutionKind => named::<ResolutionKind>(&name).map(FieldValue::ResolutionKind),
        F::TodoRefinement => named::<TodoRefinement>(&name).map(FieldValue::TodoRefinement),
        F::CreatedVia => named::<CreatedVia>(&name).map(FieldValue::CreatedVia),
        F::ArchivedReason => named::<ArchiveReason>(&name).map(FieldValue::ArchiveReason),
        F::RemoteLinkLinkState => named::<LinkState>(&name).map(FieldValue::LinkState),
        F::Priority | F::EffectivePriority | F::LocalPriorityOverride => {
            named::<Priority>(&name).map(FieldValue::Priority)
        }
        F::Energy => named::<Energy>(&name).map(FieldValue::Energy),
        _ => None,
    };
    value.ok_or_else(|| {
        let names = enum_names(field);
        format!(
            "{name:?} is not a {}: use {}",
            field.path(),
            names.join(", ")
        )
    })
}

/// The values an enum target takes, in proto order, for messages.
fn enum_names(field: ItemField) -> Vec<&'static str> {
    use ItemField as F;
    match field {
        F::Status => vec![
            "inbox",
            "next",
            "someday",
            "in_progress",
            "done",
            "cancelled",
        ],
        F::StatusCategory => vec!["todo", "in_progress", "done"],
        F::ResolutionKind => vec!["done", "cancelled"],
        F::TodoRefinement => vec!["inbox", "next", "someday"],
        F::CreatedVia => vec![
            "tui",
            "cli",
            "mcp",
            "remote_import",
            "restore",
            "split",
            "recurrence",
            "mobile",
        ],
        F::ArchivedReason => vec!["user", "left_scope", "remote_unavailable", "auto_done"],
        F::RemoteLinkLinkState => vec!["pending", "ok", "broken"],
        F::Energy => vec!["none", "low", "medium", "high"],
        _ => vec!["none", "lowest", "low", "medium", "high", "highest"],
    }
}

// ---- printing --------------------------------------------------------------

/// The canonical text of a query: the filter's `all_of` terms with its
/// `any_of` as one `any:( … )` before the first other `any:`, then
/// `include:`, `sort:` and `count:off`. [`parse_query`] reads it back to
/// the same query whenever text could have written it. A filter only the
/// API can build reads back as one that matches the same Items: an `any:`
/// group in `all_of` while `any_of` is empty becomes the `any_of`; a group
/// with both `all_of` and `any_of` is written `( … any:( … ))`, one group
/// deeper, so at the depth limit it reads back with an error instead.
pub fn print_query(query: &Query) -> String {
    let f = &query.filter;
    // The filter's any_of goes before the first any:( … ) of its all_of,
    // since the first one in the text is read back as the any_of.
    let mut clauses: Vec<Printed<'_>> = Vec::new();
    let mut any_of = (!f.any_of.is_empty()).then_some(&f.any_of[..]);
    for c in &f.all_of {
        if pure_any(c)
            && let Some(any) = any_of.take()
        {
            clauses.push(Printed::Any(any));
        }
        clauses.push(Printed::Clause(c));
    }
    if let Some(any) = any_of {
        clauses.push(Printed::Any(any));
    }
    let mut out = print_list(&clauses);
    let include: Vec<&str> = [
        (f.include_archived, "archived"),
        (f.include_done, "done"),
        (f.include_deferred, "deferred"),
        (f.include_deleted, "deleted"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect();
    if !include.is_empty() {
        out.push(format!("include:{}", include.join(",")));
    }
    if !query.sort.is_empty() {
        let keys: Vec<String> = query
            .sort
            .iter()
            .map(|k| format!("{}{}", if k.descending { "-" } else { "" }, k.key.path()))
            .collect();
        out.push(format!("sort:{}", keys.join(",")));
    }
    if !query.show_count {
        out.push("count:off".to_owned());
    }
    out.join(" ")
}

/// One printed term: a clause, or a list of alternatives as `any:( … )`.
enum Printed<'a> {
    Clause(&'a Clause),
    Any(&'a [Clause]),
}

/// Whether `c` is a group of alternatives only, printed as `any:( … )`.
fn pure_any(c: &Clause) -> bool {
    matches!(c, Clause::Group(g) if g.all_of.is_empty() && !g.any_of.is_empty())
}

/// Terms side by side. A title MATCHES right after another is written
/// `title~…`, since side-by-side free text reads back as one MATCHES.
fn print_list(terms: &[Printed<'_>]) -> Vec<String> {
    let mut out = Vec::with_capacity(terms.len());
    let mut after_text = false;
    for term in terms {
        let (text, is_text) = match term {
            Printed::Any(any) => (print_any(any), false),
            Printed::Clause(c) => print_clause(c, after_text),
        };
        out.push(text);
        after_text = is_text;
    }
    out
}

fn print_any(clauses: &[Clause]) -> String {
    let terms: Vec<Printed<'_>> = clauses.iter().map(Printed::Clause).collect();
    format!("any:({})", print_list(&terms).join(" "))
}

/// The clause's text, and whether it is free title text, which a title
/// MATCHES next to it would join (`after_text`: one comes right before it).
fn print_clause(c: &Clause, after_text: bool) -> (String, bool) {
    match c {
        Clause::Predicate(p) => print_predicate(p, after_text),
        Clause::Group(g) => {
            let text = if pure_any(c) {
                print_any(&g.any_of)
            } else {
                let mut terms: Vec<Printed<'_>> = g.all_of.iter().map(Printed::Clause).collect();
                if !g.any_of.is_empty() {
                    terms.push(Printed::Any(&g.any_of));
                }
                format!("({})", print_list(&terms).join(" "))
            };
            (text, false)
        }
    }
}

fn print_predicate(p: &Predicate, after_text: bool) -> (String, bool) {
    if p.target == ItemField::Title
        && p.op == Operator::Matches
        && !after_text
        && let [Operand::Text(t)] = p.operands.as_slice()
    {
        let text = if plain_words(t) { t.clone() } else { quote(t) };
        return (text, true);
    }
    (print_term(p), false)
}

fn print_term(p: &Predicate) -> String {
    use Operator as O;
    let name = p.target.path();
    let values = || {
        p.operands
            .iter()
            .map(print_operand)
            .collect::<Vec<_>>()
            .join(",")
    };
    match p.op {
        O::IsSet => format!("has:{name}"),
        O::IsUnset => format!("no:{name}"),
        O::In if p.target == ItemField::Status => format!("is:{}", values()),
        O::Matches => match p.operands.as_slice() {
            [Operand::Text(t)] => format!("{name}~{}", word_or_quote(t)),
            _ => format!("{name}~{}", values()),
        },
        O::ContainsAny if p.target == ItemField::Labels && p.operands.len() == 1 => {
            match p.operands.as_slice() {
                [Operand::Value(FieldValue::String(l))] if !needs_quotes(l) && !l.is_empty() => {
                    format!("#{l}")
                }
                _ => format!("{name}:{}", values()),
            }
        }
        O::ContainsAny => format!("{name}:{}", values()),
        O::ContainsAll => format!("{name}:all={}", values()),
        O::Eq => format!("{name}:={}", values()),
        O::In => format!("{name}:{}", values()),
        O::Ne => format!("{name}!={}", values()),
        O::Lt => format!("{name}<{}", values()),
        O::Le => format!("{name}<={}", values()),
        O::Gt => format!("{name}>{}", values()),
        O::Ge => format!("{name}>={}", values()),
    }
}

fn print_operand(o: &Operand) -> String {
    match o {
        Operand::RelativeDate(d) => {
            let name = match d.anchor {
                Anchor::Today => "today",
                Anchor::StartOfWeek => "week_start",
                Anchor::EndOfWeek => "week_end",
                Anchor::CurrentWeek => return "this_week".to_owned(),
            };
            match d.offset_days {
                0 => name.to_owned(),
                n if n % 7 == 0 => format!("{name}{:+}w", n / 7),
                n => format!("{name}{n:+}d"),
            }
        }
        Operand::Text(t) => word_or_quote(t),
        Operand::Value(v) => print_value(v),
    }
}

fn print_value(v: &FieldValue) -> String {
    use FieldValue as V;
    fn serde_name<T: Serialize>(v: &T) -> String {
        match serde_json::to_value(v) {
            Ok(serde_json::Value::String(s)) => s,
            _ => String::new(),
        }
    }
    match v {
        V::String(s) | V::Markdown(s) | V::IsoWeek(s) => word_or_quote(s),
        V::Person(PersonRef::FreeText(s)) => word_or_quote(s),
        V::Person(PersonRef::Person(id)) => format!("{PERSON_PREFIX}{id}"),
        V::ItemId(id) => id.to_string(),
        V::Bool(b) => b.to_string(),
        // The shortest text that reads back, in exponent form when the
        // positional one would run long.
        V::Number(n) => {
            let mut s = String::new();
            let magnitude = n.abs();
            if magnitude != 0.0 && !(1e-5..1e16).contains(&magnitude) {
                let _ = write!(s, "{n:e}");
            } else {
                let _ = write!(s, "{n}");
            }
            s
        }
        V::Uint(n) => n.to_string(),
        V::Date(d) => d.to_string(),
        V::Timestamp(t) => t.to_string(),
        // jiff's friendly form without spaces (2h30m); a negative one is
        // that with a sign, not jiff's "ago".
        V::Duration(d) if d.is_negative() => match d.checked_neg() {
            Some(abs) => format!("-{abs:#}").replace(' ', ""),
            None => d.to_string(),
        },
        V::Duration(d) => format!("{d:#}").replace(' ', ""),
        V::Priority(p) => serde_name(p),
        V::Energy(e) => serde_name(e),
        V::Status(s) => serde_name(s),
        V::StatusCategory(s) => serde_name(s),
        V::ResolutionKind(s) => serde_name(s),
        V::TodoRefinement(s) => serde_name(s),
        V::CreatedVia(s) => serde_name(s),
        V::ArchiveReason(s) => serde_name(s),
        V::LinkState(s) => serde_name(s),
        other => format!("{other:?}"),
    }
}

/// Whether `s` must be quoted to read back as one value: it holds a space,
/// a quote, a comma or a parenthesis, or an operator character, which next
/// to the operator before it would read as part of it (`title:=x`).
fn needs_quotes(s: &str) -> bool {
    s.is_empty()
        || s.contains(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '"' | ',' | '(' | ')' | '\\' | ':' | '<' | '>' | '=' | '!' | '~'
                )
        })
}

fn word_or_quote(s: &str) -> String {
    if needs_quotes(s) {
        quote(s)
    } else {
        s.to_owned()
    }
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Whether free title text reads back as these exact words: single spaces
/// between bare words that no other term could be mistaken for.
fn plain_words(t: &str) -> bool {
    !t.is_empty()
        && t.split(' ')
            .all(|w| !w.is_empty() && !needs_quotes(w) && !w.starts_with(['#', '-']))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::defaults;

    fn parse(text: &str) -> Query {
        let parsed = parse_query(text);
        assert_eq!(parsed.errors, vec![], "{text}");
        parsed.query
    }

    fn errors(text: &str) -> Vec<(usize, usize, String)> {
        parse_query(text)
            .errors
            .into_iter()
            .map(|e| (e.start, e.end, e.message))
            .collect()
    }

    const DEFAULTS: [&str; 6] = [
        "is:inbox sort:created_at",
        "any:(due_date<=today is:in_progress (has:waiting waiting.follow_up_on<=today) focus_week:=this_week) sort:-is_overdue,-effective_priority,due_date",
        "is:next,in_progress sort:-effective_priority,due_date",
        "has:waiting sort:waiting.since",
        "is:someday sort:-updated_at count:off",
        "any:(has:archived is:done,cancelled) include:archived,done,deferred sort:-updated_at count:off",
    ];

    #[test]
    fn the_defaults_print_and_parse_back() {
        for (view, text) in defaults().into_iter().zip(DEFAULTS) {
            let query = Query {
                filter: view.filter,
                sort: view.sort,
                show_count: view.show_count,
            };
            assert_eq!(print_query(&query), text, "{}", view.name);
            assert_eq!(parse(text), query, "{}", view.name);
        }
    }

    #[test]
    fn terms_lower_to_predicates() {
        use FieldValue as V;
        use ItemField as F;
        use Operator as O;
        let pred = |q: &Query| match &q.filter.all_of[..] {
            [Clause::Predicate(p)] => p.clone(),
            other => panic!("{other:?}"),
        };
        let v = |x: V| Operand::Value(x);
        let cases: Vec<(&str, Predicate)> = vec![
            (
                "is:next",
                Predicate::new(F::Status, O::In, vec![v(V::Status(Status::Next))]),
            ),
            (
                "-is:next",
                Predicate::new(F::Status, O::Ne, vec![v(V::Status(Status::Next))]),
            ),
            (
                "#work",
                Predicate::new(F::Labels, O::ContainsAny, vec![v(V::String("work".into()))]),
            ),
            (
                "labels:a,b",
                Predicate::new(
                    F::Labels,
                    O::ContainsAny,
                    vec![v(V::String("a".into())), v(V::String("b".into()))],
                ),
            ),
            (
                "labels:all=a,b",
                Predicate::new(
                    F::Labels,
                    O::ContainsAll,
                    vec![v(V::String("a".into())), v(V::String("b".into()))],
                ),
            ),
            (
                "priority>=high",
                Predicate::new(
                    F::Priority,
                    O::Ge,
                    vec![v(V::Priority(crate::item::Priority::High))],
                ),
            ),
            (
                "energy:low",
                Predicate::new(
                    F::Energy,
                    O::In,
                    vec![v(V::Energy(crate::item::Energy::Low))],
                ),
            ),
            (
                "energy>=medium",
                Predicate::new(
                    F::Energy,
                    O::Ge,
                    vec![v(V::Energy(crate::item::Energy::Medium))],
                ),
            ),
            (
                "due_date<today+3d",
                Predicate::new(
                    F::DueDate,
                    O::Lt,
                    vec![RelativeDate::new(Anchor::Today, 3).into()],
                ),
            ),
            (
                "due_date>=week_start-2w",
                Predicate::new(
                    F::DueDate,
                    O::Ge,
                    vec![RelativeDate::new(Anchor::StartOfWeek, -14).into()],
                ),
            ),
            (
                "defer_until:=2026-11-02",
                Predicate::new(
                    F::DeferUntil,
                    O::Eq,
                    vec![v(V::Date(jiff::civil::date(2026, 11, 2)))],
                ),
            ),
            (
                "has:defer_until",
                Predicate::new(F::DeferUntil, O::IsSet, vec![]),
            ),
            (
                "-has:waiting",
                Predicate::new(F::Waiting, O::IsUnset, vec![]),
            ),
            ("no:waiting", Predicate::new(F::Waiting, O::IsUnset, vec![])),
            (
                "waiting.on:\"the vendor\"",
                Predicate::new(
                    F::WaitingOn,
                    O::In,
                    vec![v(V::Person(PersonRef::FreeText("the vendor".into())))],
                ),
            ),
            (
                "description~\"flaky test\"",
                Predicate::new(
                    F::Description,
                    O::Matches,
                    vec![Operand::Text("flaky test".into())],
                ),
            ),
            (
                "milk bread",
                Predicate::new(
                    F::Title,
                    O::Matches,
                    vec![Operand::Text("milk bread".into())],
                ),
            ),
            (
                "\"a:b c\"",
                Predicate::new(F::Title, O::Matches, vec![Operand::Text("a:b c".into())]),
            ),
            (
                "is_overdue:=true",
                Predicate::new(F::IsOverdue, O::Eq, vec![v(V::Bool(true))]),
            ),
            (
                "story_points>2.5",
                Predicate::new(F::StoryPoints, O::Gt, vec![v(V::Number(2.5))]),
            ),
            (
                "original_estimate<=2h30m",
                Predicate::new(
                    F::OriginalEstimate,
                    O::Le,
                    vec![v(V::Duration(SignedDuration::from_mins(150)))],
                ),
            ),
            (
                "focus_week:2026-W45",
                Predicate::new(F::FocusWeek, O::In, vec![v(V::IsoWeek("2026-W45".into()))]),
            ),
            (
                "archived.reason:auto_done",
                Predicate::new(
                    F::ArchivedReason,
                    O::In,
                    vec![v(V::ArchiveReason(crate::item::ArchiveReason::AutoDone))],
                ),
            ),
            (
                "Status:NEXT",
                Predicate::new(F::Status, O::In, vec![v(V::Status(Status::Next))]),
            ),
        ];
        for (text, want) in cases {
            assert_eq!(pred(&parse(text)), want, "{text}");
        }
    }

    #[test]
    fn groups_flags_and_sort() {
        let q = parse(
            "#work any:(is:next has:waiting) (priority:high any:(due_date<=today)) include:done,deleted sort:-priority,due_date count:off",
        );
        assert_eq!(q.filter.all_of.len(), 2);
        assert_eq!(q.filter.any_of.len(), 2);
        assert!(q.filter.include_done && q.filter.include_deleted);
        assert!(!q.filter.include_archived && !q.filter.include_deferred);
        assert_eq!(
            q.sort,
            [
                SortKey::desc(ItemField::Priority),
                SortKey::asc(ItemField::DueDate)
            ]
        );
        assert!(!q.show_count);
        // A second top-level any: is a group of the all_of.
        let q = parse("any:(is:next) any:(is:inbox)");
        assert_eq!((q.filter.any_of.len(), q.filter.all_of.len()), (1, 1));
        assert_eq!(parse("").filter, Filter::default());
    }

    #[test]
    fn errors_name_their_span() {
        assert_eq!(
            errors("is:next dew:today"),
            [(8, 17, "unknown field \"dew\"".to_owned())]
        );
        assert_eq!(
            errors("energy:huge"),
            [(
                0,
                11,
                "\"huge\" is not a energy: use none, low, medium, high".to_owned()
            )]
        );
        assert_eq!(
            errors("energy>none"),
            [(0, 11, "ENERGY_NONE is outside the energy order".to_owned())]
        );
        assert_eq!(
            errors("priority:urgent"),
            [(
                0,
                15,
                "\"urgent\" is not a priority: use none, lowest, low, medium, high, highest"
                    .to_owned()
            )]
        );
        assert_eq!(
            errors("-#work"),
            [(
                0,
                6,
                "a label cannot be negated: a filter has no NOT".to_owned()
            )]
        );
        assert_eq!(
            errors("-due_date<today")[0].2,
            "only field:value and has: can be negated (-f:v is f!=v): a filter has no NOT"
        );
        assert_eq!(errors("(is:next"), [(0, 8, "a ( is not closed".to_owned())]);
        assert_eq!(
            errors("is:next )"),
            [(8, 9, "a ) without its (".to_owned())]
        );
        assert_eq!(
            errors("\"milk"),
            [(0, 5, "a quote is not closed".to_owned())]
        );
        assert_eq!(
            errors("count:maybe"),
            [(0, 11, "count: takes on or off".to_owned())]
        );
        assert_eq!(
            errors("include:all")[0].2,
            "include: takes archived, done, deferred and deleted, not \"all\""
        );
        assert_eq!(errors("sort:a sort:b").len(), 2);
        assert_eq!(
            errors("due_date<tomorrow")[0].2,
            "\"tomorrow\" is not a date: use YYYY-MM-DD, today, week_start or week_end, with +3d or -2w"
        );
        // Several errors are all reported, in text order.
        assert_eq!(errors("dew:1 is:nope").len(), 2);
        // ü is one code point.
        assert_eq!(errors("ü dew:1")[0].0, 2);
    }

    #[test]
    fn validator_errors_point_at_their_term() {
        // Ordering on a status: valid syntax, invalid filter.
        assert_eq!(
            errors("#a status<done"),
            [(
                3,
                14,
                "LT does not apply to enum targets, which have no order".to_owned()
            )]
        );
        assert_eq!(
            errors("any:(is:next title<b)"),
            [(
                13,
                20,
                "LT does not apply to string targets, which have no order".to_owned()
            )]
        );
        assert_eq!(errors("(is:next (title<b))")[0].0, 10);
        assert_eq!(
            errors("has:title")[0].2,
            "IS_SET applies only to a target that can be unset; use EQ"
        );
        assert_eq!(
            errors("sort:labels"),
            [(
                0,
                11,
                "is not a sort key: set targets have no order".to_owned()
            )]
        );
        assert_eq!(errors("sort:due_date,due_date")[0].2, "repeats sort[0].key");
    }

    /// `query` printed reads back to itself, and printing that again gives
    /// the same text.
    fn round_trips(query: &Query) {
        let text = print_query(query);
        let back = parse_query(&text);
        assert_eq!(back.errors, vec![], "printed as {text}");
        assert_eq!(&back.query, query, "printed as {text}");
        assert_eq!(print_query(&back.query), text);
    }

    fn one(p: Predicate) -> Query {
        Query {
            filter: Filter {
                all_of: vec![p.into()],
                ..Filter::default()
            },
            ..Query::default()
        }
    }

    #[test]
    fn values_that_look_like_operators_are_quoted() {
        use FieldValue as V;
        use ItemField as F;
        use Operator as O;
        let s = |x: &str| Operand::Value(V::String(x.into()));
        for (field, op, operands) in [
            (F::Sprint, O::In, vec![s("=x")]),
            (F::Labels, O::ContainsAny, vec![s("all=x"), s("y")]),
            (F::Labels, O::ContainsAny, vec![s("=x"), s("y")]),
            (F::Sprint, O::In, vec![s("all=x")]),
            (F::RemoteLinkUrl, O::Eq, vec![s("https://x.example/a?b=c")]),
            (F::Title, O::Ne, vec![s("~x!")]),
            (F::Sprint, O::Eq, vec![s("=")]),
        ] {
            round_trips(&one(Predicate::new(field, op, operands)));
        }
        let person = |x: &str| Operand::Value(V::Person(PersonRef::FreeText(x.into())));
        round_trips(&one(Predicate::new(
            F::WaitingOn,
            O::In,
            vec![person("=bob")],
        )));
    }

    #[test]
    fn side_by_side_title_matches_stay_apart() {
        let m = |t: &str| {
            Clause::from(Predicate::new(
                ItemField::Title,
                Operator::Matches,
                vec![Operand::Text(t.into())],
            ))
        };
        let any = Query {
            filter: Filter {
                any_of: vec![m("milk"), m("bread")],
                ..Filter::default()
            },
            ..Query::default()
        };
        assert_eq!(print_query(&any), "any:(milk title~bread)");
        round_trips(&any);
        let long = "a".repeat(600);
        let all = Query {
            filter: Filter {
                all_of: vec![m(&long), m(&long)],
                ..Filter::default()
            },
            ..Query::default()
        };
        round_trips(&all);
    }

    #[test]
    fn person_ids_durations_numbers_and_offsets_read_back() {
        use FieldValue as V;
        use ItemField as F;
        use Operator as O;
        let id: crate::ids::PersonId = "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap();
        let person = Predicate::new(
            F::Assignee,
            O::Eq,
            vec![V::Person(PersonRef::Person(id)).into()],
        );
        assert_eq!(
            print_query(&one(person.clone())),
            "assignee:=person:01ARZ3NDEKTSV4RRFFQ69G5FAV"
        );
        round_trips(&one(person));
        round_trips(&one(Predicate::new(
            F::WaitingOn,
            O::In,
            vec![V::Person(PersonRef::FreeText("person:x".into())).into()],
        )));
        for d in [
            SignedDuration::from_secs(-3600),
            SignedDuration::from_millis(-1500),
            SignedDuration::from_nanos(1),
        ] {
            round_trips(&one(Predicate::new(
                F::OriginalEstimate,
                O::Lt,
                vec![V::Duration(d).into()],
            )));
        }
        for n in [1e300, 5e-324, -0.0, 2.0, 1234567.5, -1e-7] {
            let q = one(Predicate::new(
                F::StoryPoints,
                O::Lt,
                vec![V::Number(n).into()],
            ));
            assert!(print_query(&q).len() < 40, "{}", print_query(&q));
            round_trips(&q);
        }
        for days in [i32::MIN, i32::MAX, -7, 13] {
            round_trips(&one(Predicate::new(
                F::DueDate,
                O::Lt,
                vec![RelativeDate::new(Anchor::Today, days).into()],
            )));
        }
    }

    #[test]
    fn two_top_level_any_groups_are_a_fixed_point() {
        let q = parse("any:(is:next is:inbox) any:(#a #b)");
        assert_eq!(print_query(&q), "any:(is:next is:inbox) any:(#a #b)");
        round_trips(&q);
        let q = parse("#x any:(#a #b) any:(is:next)");
        round_trips(&q);
    }

    #[test]
    fn deep_nesting_is_an_error_not_a_stack_overflow() {
        let deep = "(".repeat(MAX_QUERY_BYTES);
        let errs = errors(&deep);
        assert!(!errs.is_empty());
        let nested = format!("{}is:next{}", "(".repeat(6), ")".repeat(6));
        assert_eq!(
            errors(&nested),
            [(4, 15, "groups nest at most 4 deep".to_owned())]
        );
        // Four deep is fine.
        assert_eq!(
            errors(&format!("{}is:next{}", "(".repeat(4), ")".repeat(4))),
            vec![]
        );
    }

    #[test]
    fn a_stray_close_is_one_error_and_parsing_goes_on() {
        assert_eq!(
            errors("a ) dew:1"),
            [
                (2, 3, "a ) without its (".to_owned()),
                (4, 9, "unknown field \"dew\"".to_owned()),
            ]
        );
    }

    #[test]
    fn negation_errors_say_why() {
        assert_eq!(
            errors("-labels:a")[0].2,
            "labels is a set, which cannot be negated: a filter has no NOT"
        );
        assert_eq!(
            errors("-is:next,done")[0].2,
            "only one value can be negated (-f:v is f!=v): a filter has no NOT"
        );
    }

    #[test]
    fn text_over_the_limit_is_one_error() {
        let long = "a ".repeat(MAX_QUERY_BYTES);
        assert_eq!(errors(&long).len(), 1);
    }

    /// Printing then parsing gives back the query, for every target and
    /// each operator and operand kind it takes.
    #[test]
    fn printed_queries_parse_back() {
        use crate::item::{ArchiveReason, Energy, Priority};
        use FieldValue as V;
        use Operator as O;
        let values = |field: ItemField| -> Vec<Operand> {
            use ValueClass as C;
            match target(field).class {
                C::Enum => match field {
                    ItemField::Status => vec![V::Status(Status::InProgress).into()],
                    ItemField::ArchivedReason => {
                        vec![V::ArchiveReason(ArchiveReason::LeftScope).into()]
                    }
                    _ => vec![],
                },
                C::Priority => vec![V::Priority(Priority::Lowest).into()],
                C::Energy => vec![V::Energy(Energy::Medium).into()],
                C::String | C::Set => vec![
                    V::String("a b,c\"d".into()).into(),
                    V::String("x".into()).into(),
                ],
                C::Date => vec![
                    V::Date(jiff::civil::date(2026, 2, 28)).into(),
                    RelativeDate::new(Anchor::EndOfWeek, -3).into(),
                    RelativeDate::new(Anchor::Today, 14).into(),
                ],
                C::FocusWeek => vec![V::IsoWeek("2026-W01".into()).into()],
                C::Number => vec![V::Number(-0.25).into()],
                C::Duration => vec![V::Duration(SignedDuration::from_secs(3_725)).into()],
                C::Bool => vec![V::Bool(false).into()],
                C::Instant => vec![V::Timestamp("2026-11-02T10:00:00.5Z".parse().unwrap()).into()],
                C::Uint => vec![V::Uint(42).into()],
                C::Person => vec![V::Person(PersonRef::FreeText("Dana K".into())).into()],
                _ => vec![],
            }
        };
        let mut n = 0;
        for &field in ItemField::ALL {
            let t = target(field);
            let mut preds = Vec::new();
            if t.can_unset || matches!(t.class, ValueClass::Set) {
                preds.push(Predicate::new(field, O::IsSet, vec![]));
                preds.push(Predicate::new(field, O::IsUnset, vec![]));
            }
            if t.matches {
                preds.push(Predicate::new(
                    field,
                    O::Matches,
                    vec![Operand::Text("two words".into())],
                ));
                preds.push(Predicate::new(
                    field,
                    O::Matches,
                    vec![Operand::Text("x:y".into())],
                ));
            }
            for v in values(field) {
                if t.class == ValueClass::Set {
                    preds.push(Predicate::new(field, O::ContainsAny, vec![v.clone()]));
                    preds.push(Predicate::new(
                        field,
                        O::ContainsAll,
                        vec![v.clone(), V::String("z".into()).into()],
                    ));
                    continue;
                }
                for op in [O::Eq, O::Ne, O::In] {
                    preds.push(Predicate::new(field, op, vec![v.clone()]));
                }
                preds.push(Predicate::new(field, O::In, vec![v.clone(), v.clone()]));
                if t.class.is_ordered() {
                    for op in [O::Lt, O::Le, O::Gt, O::Ge] {
                        preds.push(Predicate::new(field, op, vec![v.clone()]));
                    }
                }
            }
            for p in preds {
                let query = Query {
                    filter: Filter {
                        all_of: vec![p.clone().into()],
                        ..Filter::default()
                    },
                    ..Query::default()
                };
                if query.filter.validate().is_err() {
                    continue;
                }
                let text = print_query(&query);
                let back = parse_query(&text);
                assert_eq!(back.errors, vec![], "{p:?} printed as {text}");
                assert_eq!(back.query, query, "{p:?} printed as {text}");
                n += 1;
            }
        }
        assert!(n > 200, "only {n} cases");
    }
}
