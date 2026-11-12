//! Jira wiki markup ↔ Markdown, for descriptions (ADR-0009). Data Center
//! stores a description as wiki markup; an Item holds Markdown. The common
//! constructs convert: headings, lists, bold, monospace, links, code blocks
//! and simple tables. The rest is kept as it is, never dropped: as text when
//! it reads as text (`_italic_`, `{color:#888}…{color}`), else as a `wiki:`
//! code span or a ```` ```wiki ```` block, which convert back verbatim.
//!
//! [`to_markdown`] checks each line it converts: one that would not come
//! back as it was is kept raw instead. So `to_wiki(&to_markdown(w))` is
//! `canonical(w)` for every `w`, and an unedited description pushes nothing
//! new.

/// `wiki` as Jira renders it alike: `\n` line ends, no trailing spaces or
/// tabs, no trailing blank lines, and a list item's marker at its line's
/// start. Non-breaking spaces stay.
pub fn canonical(wiki: &str) -> String {
    let text = wiki.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<&str> = text
        .split('\n')
        .map(|l| {
            let l = l.trim_end_matches([' ', '\t']);
            let start = l.trim_start_matches([' ', '\t']);
            if start.len() < l.len() && list_item(start).is_some() {
                start
            } else {
                l
            }
        })
        .collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// A wiki list item: its marker (`*`, `**`, `#`, `*#`, or a lone `-`) and
/// its text, after the one space the marker needs.
fn list_item(line: &str) -> Option<(&str, &str)> {
    let end = line.find(|c| c != '*' && c != '#').unwrap_or(line.len());
    let (marker, rest) = if end == 0 && line.starts_with('-') {
        line.split_at(1)
    } else {
        line.split_at(end)
    };
    let text = rest.strip_prefix(' ')?;
    (!marker.is_empty()).then_some((marker, text))
}

/// The Markdown of a wiki description (see the module docs).
pub fn to_markdown(wiki: &str) -> String {
    let canon = canonical(wiki);
    if canon.is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = canon.split('\n').collect();
    let mut out: Vec<String> = Vec::new();
    // Lines that do not convert, kept for one ```wiki block.
    let mut raw: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let region = match (macro_end(&lines, i), table_end(&lines, i)) {
            (Some(n), _) => Some((n, code_block(&lines[i..n]))),
            (None, Some(n)) => Some((n, table(&lines[i..n]))),
            _ => None,
        };
        if let Some((n, md)) = region {
            match md {
                Some(md) if to_wiki(&md) == lines[i..n].join("\n") => {
                    flush(&mut out, &mut raw);
                    out.push(md);
                }
                _ => raw.extend(&lines[i..n]),
            }
            i = n;
            continue;
        }
        let line = lines[i];
        let md = line_to_markdown(line);
        if to_wiki(&md) == line {
            flush(&mut out, &mut raw);
            out.push(md);
        } else if !line.contains('`') {
            flush(&mut out, &mut raw);
            out.push(format!("`wiki:{line}`"));
        } else {
            raw.push(line);
        }
        i += 1;
    }
    flush(&mut out, &mut raw);
    let md = out.join("\n");
    // A last check of the whole: should lines read together differ from
    // lines read alone, all of it is kept raw.
    if to_wiki(&md) == canon {
        md
    } else {
        raw_block(&lines)
    }
}

/// `raw` as one ```wiki block in `out`, if there is any.
fn flush(out: &mut Vec<String>, raw: &mut Vec<&str>) {
    if !raw.is_empty() {
        out.push(raw_block(raw));
        raw.clear();
    }
}

/// `lines` kept verbatim: a ```wiki block, its fence longer than any run of
/// backticks inside.
fn raw_block(lines: &[&str]) -> String {
    let fence = fence_for(lines);
    format!("{fence}wiki\n{}\n{fence}", lines.join("\n"))
}

/// A code fence longer than any run of backticks in `lines`, three at least.
fn fence_for(lines: &[&str]) -> String {
    let longest = lines
        .iter()
        .flat_map(|l| l.split(|c| c != '`'))
        .map(str::len)
        .max()
        .unwrap_or(0);
    "`".repeat((longest + 1).max(3))
}

/// The tags whose region spans lines: code, noformat, panel and quote.
const MACROS: [&str; 4] = ["code", "noformat", "panel", "quote"];

/// Where a `{code}` (or noformat, panel, quote) region starting at line `i`
/// ends (exclusive): at the line holding its closing tag.
fn macro_end(lines: &[&str], i: usize) -> Option<usize> {
    let line = lines[i];
    let tag = MACROS.iter().find(|t| {
        line.strip_prefix('{')
            .and_then(|r| r.strip_prefix(**t))
            .is_some_and(|r| r.starts_with(':') || r.starts_with('}'))
    })?;
    let close = format!("{{{tag}}}");
    let opened = line.find('}')? + 1;
    if line[opened..].contains(&close) {
        return Some(i + 1);
    }
    (i + 1..lines.len())
        .find(|&j| lines[j].contains(&close))
        .map(|j| j + 1)
}

/// A `{code}` or `{code:lang}` block alone on its lines, as a fenced block.
/// One closed at the end of its last line (`text{code}`) converts only as
/// part of the whole check: its fence puts the close on a line of its own,
/// which Jira renders alike, so [`to_markdown`] keeps it raw.
fn code_block(region: &[&str]) -> Option<String> {
    let [open, body @ .., close] = region else {
        return None;
    };
    let lang = match *open {
        "{code}" => "",
        o => o.strip_prefix("{code:")?.strip_suffix('}')?,
    };
    let simple = lang
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_+#-".contains(c));
    if *close != "{code}" || !simple || lang == "wiki" || lang.starts_with('`') {
        return None;
    }
    let fence = fence_for(body);
    let body: String = body.iter().map(|l| format!("{l}\n")).collect();
    Some(format!("{fence}{lang}\n{body}{fence}"))
}

/// Where a table starting at line `i` ends (exclusive): its rows are the
/// lines that start with `|`.
fn table_end(lines: &[&str], i: usize) -> Option<usize> {
    if !lines[i].starts_with('|') {
        return None;
    }
    Some(
        (i..lines.len())
            .find(|&j| !lines[j].starts_with('|'))
            .unwrap_or(lines.len()),
    )
}

/// The cells of a wiki table row: `||a||b||` (a heading row) or `|a|b|`.
fn cells(row: &str) -> Option<(bool, Vec<&str>)> {
    let (head, sep) = if row.starts_with("||") {
        (true, "||")
    } else {
        (false, "|")
    };
    let inner = row.strip_prefix(sep)?.strip_suffix(sep)?;
    let cells: Vec<&str> = inner.split(sep).collect();
    // A cell that needs a pipe or a code span of its own stays raw.
    let plain = cells
        .iter()
        .all(|c| !c.contains('|') && !c.contains('`') && !c.is_empty());
    plain.then_some((head, cells))
}

/// A table of one heading row and plain rows, as a GFM pipe table.
fn table(rows: &[&str]) -> Option<String> {
    let (head, first) = cells(rows[0])?;
    if !head {
        return None;
    }
    let width = first.len();
    let mut out = vec![
        md_row(&first),
        format!("| {} |", vec!["---"; width].join(" | ")),
    ];
    for row in &rows[1..] {
        let (head, cells) = cells(row)?;
        if head || cells.len() != width {
            return None;
        }
        out.push(md_row(&cells));
    }
    Some(out.join("\n"))
}

/// A table row's cells, each converted inline.
fn md_row(cells: &[&str]) -> String {
    let cells: Vec<String> = cells.iter().map(|c| inline_to_markdown(c)).collect();
    format!("| {} |", cells.join(" | "))
}

/// One line of wiki that is not in a code block or table, as Markdown:
/// a heading, a list item or a paragraph line.
fn line_to_markdown(line: &str) -> String {
    if let Some((level, text)) = heading(line) {
        return format!("{} {}", "#".repeat(level), inline_to_markdown(text));
    }
    if let Some((marker, text)) = list_item(line) {
        // `-` and a run of one kind nest; a mix (`*#`) is not Markdown's.
        let depth = marker.len();
        let bullet = match marker.chars().next() {
            Some('-') => Some("+"),
            Some('*') if marker.chars().all(|c| c == '*') => Some("-"),
            Some('#') if marker.chars().all(|c| c == '#') => Some("1."),
            _ => None,
        };
        if let Some(b) = bullet {
            let indent = "  ".repeat(depth - 1);
            return format!("{indent}{b} {}", inline_to_markdown(text));
        }
    }
    escape_block_start(&inline_to_markdown(line))
}

/// `h1. text` to `h6. text`.
fn heading(line: &str) -> Option<(usize, &str)> {
    let rest = line.strip_prefix('h')?;
    let level = rest.chars().next()?.to_digit(10)? as usize;
    let text = rest[1..].strip_prefix(". ")?;
    (1..=6).contains(&level).then_some((level, text))
}

/// A paragraph line that Markdown would read as a block of its own (a
/// heading, a list item, a quote, a fence) has its start escaped: `\#`,
/// `1\.`.
fn escape_block_start(md: &str) -> String {
    let rest = md.trim_start_matches(' ');
    let lead = &md[..md.len() - rest.len()];
    if let Some((n, tail)) = rest.split_once(". ")
        && !n.is_empty()
        && n.chars().all(|c| c.is_ascii_digit())
    {
        return format!("{lead}{n}\\. {tail}");
    }
    let starts = ["#", "-", "+", ">", "|", "~~~", "* "];
    if starts.iter().any(|s| rest.starts_with(s)) {
        format!("{lead}\\{rest}")
    } else {
        md.to_owned()
    }
}

/// Whether `s` is a link's address: a URL with a scheme, or mailto.
fn is_url(s: &str) -> bool {
    (s.contains("://") || s.starts_with("mailto:"))
        && !s
            .chars()
            .any(|c| c.is_whitespace() || "()<>|[]`\\".contains(c))
}

/// Text with nothing Markdown or wiki would read as markup: what bold and a
/// link's text may hold.
fn is_plain(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(|c| "*`\\[]<{}|".contains(c))
}

/// Inline wiki as Markdown: `*bold*`, `{{mono}}`, `[text|url]` and `[url]`
/// convert; Markdown's own characters are escaped; the rest is text.
fn inline_to_markdown(wiki: &str) -> String {
    let chars: Vec<char> = wiki.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let rest: String = chars[i..].iter().collect();
        if let Some(inner) = rest
            .strip_prefix("{{")
            .and_then(|r| r.split_once("}}"))
            .map(|(c, _)| c)
            && !inner.is_empty()
            && !inner.contains('`')
            && !inner.starts_with("wiki:")
            && inner.trim() == inner
        {
            out.push_str(&format!("`{inner}`"));
            i += inner.chars().count() + 4;
            continue;
        }
        if chars[i] == '*'
            && let Some(len) = bold_len(&chars, i)
        {
            let inner: String = chars[i + 1..i + 1 + len].iter().collect();
            out.push_str(&format!("**{inner}**"));
            i += len + 2;
            continue;
        }
        if chars[i] == '['
            && let Some((md, len)) = link(&rest)
        {
            out.push_str(&md);
            i += len;
            continue;
        }
        match chars[i] {
            c @ ('\\' | '`' | '*' | '[' | ']' | '<') => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
        i += 1;
    }
    out
}

/// The length of a `*bold*`'s text at `chars[at]`, its `*` at a word's
/// edge on both sides.
fn bold_len(chars: &[char], at: usize) -> Option<usize> {
    if at > 0 && chars[at - 1].is_alphanumeric() {
        return None;
    }
    let close = (at + 1..chars.len()).find(|&j| chars[j] == '*')?;
    let inner: String = chars[at + 1..close].iter().collect();
    let edge = chars.get(close + 1).is_none_or(|c| !c.is_alphanumeric());
    (edge && is_plain(&inner) && inner.trim() == inner).then_some(close - at - 1)
}

/// A wiki link at the start of `s`, `[text|url]` or `[url]`, as Markdown,
/// with the bytes it took.
fn link(s: &str) -> Option<(String, usize)> {
    let close = s.find(']')?;
    let body = &s[1..close];
    let md = match body.split_once('|') {
        Some((text, url)) if is_plain(text) && !text.contains('(') && is_url(url) => {
            format!("[{text}]({url})")
        }
        None if is_url(body) => format!("<{body}>"),
        _ => return None,
    };
    Some((md, s[..=close].chars().count()))
}

/// The wiki of a Markdown description: what [`to_markdown`] makes converts
/// back exactly; a person's own Markdown converts as its constructs say,
/// and any character without a meaning in Markdown is kept.
pub fn to_wiki(md: &str) -> String {
    let text = md.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some((fence, info)) = fence_open(line) {
            let end = (i + 1..lines.len()).find(|&j| lines[j].trim_end() == fence);
            let body = &lines[i + 1..end.unwrap_or(lines.len())];
            if info == "wiki" {
                out.extend(body.iter().map(|l| (*l).to_owned()));
            } else {
                out.push(if info.is_empty() {
                    "{code}".to_owned()
                } else {
                    format!("{{code:{info}}}")
                });
                out.extend(body.iter().map(|l| (*l).to_owned()));
                out.push("{code}".to_owned());
            }
            i = end.map_or(lines.len(), |e| e + 1);
            continue;
        }
        if line.starts_with('|') && lines.get(i + 1).is_some_and(|l| is_rule_row(l)) {
            out.push(wiki_row(line, true));
            i += 2;
            while i < lines.len() && lines[i].starts_with('|') {
                out.push(wiki_row(lines[i], false));
                i += 1;
            }
            continue;
        }
        out.push(line_to_wiki(line));
        i += 1;
    }
    canonical(&out.join("\n"))
}

/// A code fence's opening line: its fence and info string.
fn fence_open(line: &str) -> Option<(&str, &str)> {
    let n = line.find(|c| c != '`').unwrap_or(line.len());
    (n >= 3).then(|| (&line[..n], line[n..].trim()))
}

/// A GFM table's rule row, `| --- | --- |`.
fn is_rule_row(line: &str) -> bool {
    line.starts_with('|') && line.contains('-') && line.chars().all(|c| "|-: ".contains(c))
}

/// A GFM table row as a wiki one: a heading row with `||`.
fn wiki_row(line: &str, head: bool) -> String {
    let inner = line.trim().trim_start_matches('|').trim_end_matches('|');
    let sep = if head { "||" } else { "|" };
    let cells: Vec<String> = inner.split('|').map(|c| inline_to_wiki(c.trim())).collect();
    format!("{sep}{}{sep}", cells.join(sep))
}

/// One Markdown line, not in a fence or a table, as wiki.
fn line_to_wiki(line: &str) -> String {
    let hashes = line.find(|c| c != '#').unwrap_or(line.len());
    if (1..=6).contains(&hashes)
        && let Some(text) = line[hashes..].strip_prefix(' ')
    {
        return format!("h{hashes}. {}", inline_to_wiki(text));
    }
    let rest = line.trim_start_matches(' ');
    let depth = (line.len() - rest.len()) / 2 + 1;
    for (md, wiki) in [("- ", "*"), ("* ", "*"), ("+ ", "-"), ("1. ", "#")] {
        if let Some(text) = rest.strip_prefix(md) {
            let marker = if wiki == "-" {
                "-".to_owned()
            } else {
                wiki.repeat(depth)
            };
            return format!("{marker} {}", inline_to_wiki(text));
        }
    }
    if let Some((n, text)) = rest.split_once(". ")
        && !n.is_empty()
        && n.chars().all(|c| c.is_ascii_digit())
    {
        return format!("{} {}", "#".repeat(depth), inline_to_wiki(text));
    }
    inline_to_wiki(line)
}

/// Inline Markdown as wiki: `**bold**`, `` `mono` ``, links, the `wiki:`
/// spans kept verbatim and backslash escapes; the rest is text.
fn inline_to_wiki(md: &str) -> String {
    let chars: Vec<char> = md.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let rest: String = chars[i..].iter().collect();
        if chars[i] == '\\' && i + 1 < chars.len() && chars[i + 1].is_ascii_punctuation() {
            out.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if chars[i] == '`'
            && let Some(close) = rest[1..].find('`')
        {
            let inner = &rest[1..1 + close];
            match inner.strip_prefix("wiki:") {
                Some(raw) => out.push_str(raw),
                None => out.push_str(&format!("{{{{{inner}}}}}")),
            }
            i += inner.chars().count() + 2;
            continue;
        }
        if let Some(body) = rest.strip_prefix("**")
            && let Some(close) = body.find("**")
            && close > 0
        {
            let inner = &body[..close];
            out.push_str(&format!("*{}*", inline_to_wiki(inner)));
            i += inner.chars().count() + 4;
            continue;
        }
        if chars[i] == '['
            && let Some((wiki, len)) = md_link(&rest)
        {
            out.push_str(&wiki);
            i += len;
            continue;
        }
        if chars[i] == '<'
            && let Some(close) = rest.find('>')
            && is_url(&rest[1..close])
        {
            out.push_str(&format!("[{}]", &rest[1..close]));
            i += rest[..=close].chars().count();
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// A Markdown link at the start of `s`, `[text](url)`, as wiki, with the
/// characters it took.
fn md_link(s: &str) -> Option<(String, usize)> {
    let mid = s.find("](")?;
    let close = mid + 2 + s[mid + 2..].find(')')?;
    let (text, url) = (&s[1..mid], &s[mid + 2..close]);
    (is_url(url) && !text.contains('[')).then(|| {
        (
            format!("[{}|{url}]", inline_to_wiki(text)),
            s[..=close].chars().count(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Descriptions shaped as a real project's are (ADR-0009), with
    /// synthetic names and addresses (design 8.3).
    const SAMPLES: &[&str] = &[
        "Please:\r\n * Increase the partitions of the raw topic (probably to 100)\r\n * Create multiple replicas per type in the plugin\r\n * Test that data flows",
        "*Decision:* Adopt Release Flow. {{main}} for ongoing work, one branch per release.\n\n*Impacted teams:* Ops, Systems\n\n*Details:* [Decision page|https://wiki.example.com/pages/1]",
        "h2. Objective\n\nIntegrate the database.\n\n*Branch:* {{PLAT-1-integration}} (off dev)\n*MR:* [!211|https://git.example.com/mr/211]\n\nh3. 1. Provider\n\n- {{Provider}} Protocol (abstract)\n- {{resolve()}} resolves defaults\n\n{code}\nparameters:\n  DRY_MASS_KG:\n    type: int\n{code}\n\n||Block||Mode||Value||\n|1|{{uid}}|2880 kg|\n|2|name|1095 days|",
        "Your part of PLAT-9 (Roster: Dana Example).\n\n* Provision accesses for Dana Example\n\nClose this sub-task when done.\n\n{color:#8a8a8a}roster-task: Lead{color}",
        "h2. Implementation\r\n # Add a retry mechanism\r\n # Change after-commits to outboxes\r\n # Import pattern (Done)",
        "Enums get special treatment.\u{a0}\r\n\r\n\u{a0}\r\n\r\nThey asked:\r\n{code:java}\r\nCan I convert data to an enum?{code}\r\n\u{a0}\r\n\r\nI'd _like_ to update the schema",
        "h3. Acceptance criteria\n* Pages always show a button.\n* Offer these modes:\n** Atlas: the existing selection.\n** Flexible: choose from supported products.\n* Keep selections scoped.",
        "Improve throughput.\\n\\nNote: migrating may help.",
        "See [https://git.example.com/blob/main/x.ipynb?ref_type=heads]\r\n\r\nThe table probably does not need _all_ the columns.",
        "a *b* c 2*3*4 x*y [~dana] [^file.txt] !image.png! +under+ -strike- {panel}p{panel} bq. quote\n----\n\\\\ line",
        "`ticks` and | pipes | and <angle> and 1. not a list and # not a heading",
        "||a||b||\n|x|y|z|\n|bad|",
    ];

    #[test]
    #[ignore = "prints the samples' Markdown, to read"]
    fn show() {
        for w in SAMPLES {
            println!("----\n{}", to_markdown(w));
        }
    }

    #[test]
    fn every_sample_comes_back_as_it_was() {
        for w in SAMPLES {
            let md = to_markdown(w);
            assert_eq!(to_wiki(&md), canonical(w), "wiki: {w:?}\nmd: {md}");
            assert_eq!(to_markdown(&to_wiki(&md)), md, "md not stable: {md}");
        }
    }

    #[test]
    fn the_common_constructs_convert() {
        let md = to_markdown(SAMPLES[2]);
        for want in [
            "## Objective",
            "**Branch:** `PLAT-1-integration` (off dev)",
            "[!211](https://git.example.com/mr/211)",
            "### 1. Provider",
            "+ `Provider` Protocol (abstract)",
            "```\nparameters:\n  DRY_MASS_KG:\n    type: int\n```",
            "| Block | Mode | Value |\n| --- | --- | --- |\n| 1 | `uid` | 2880 kg |",
        ] {
            assert!(md.contains(want), "{want:?} missing from:\n{md}");
        }
        assert_eq!(
            to_markdown("h3. A\n* one\n** two\n# n"),
            "### A\n- one\n  - two\n1. n"
        );
        assert_eq!(
            to_markdown("[docs|https://x.example.com]"),
            "[docs](https://x.example.com)"
        );
        assert_eq!(
            to_markdown("[https://x.example.com]"),
            "<https://x.example.com>"
        );
    }

    #[test]
    fn what_does_not_convert_is_kept() {
        let md = to_markdown(SAMPLES[3]);
        assert!(
            md.contains("{color:#8a8a8a}roster-task: Lead{color}"),
            "{md}"
        );
        let md = to_markdown("{panel}a\nb{panel}");
        assert!(md.starts_with("```wiki\n"), "{md}");
        assert_eq!(to_wiki(&md), "{panel}a\nb{panel}");
    }

    #[test]
    fn a_persons_markdown_converts() {
        assert_eq!(
            to_wiki(
                "# Title\n\n- a **b** `c`\n  - d\n1. e\n[f](https://x.example.com) <https://y.example.com>"
            ),
            "h1. Title\n\n* a *b* {{c}}\n** d\n# e\n[f|https://x.example.com] [https://y.example.com]"
        );
        assert_eq!(
            to_wiki("```rust\nfn x() {}\n```"),
            "{code:rust}\nfn x() {}\n{code}"
        );
        assert_eq!(
            to_wiki("| a | b |\n| - | - |\n| 1 | 2 |"),
            "||a||b||\n|1|2|"
        );
        // Characters with no meaning in Markdown are kept as typed.
        assert_eq!(to_wiki("50% {braces} _x_ a|b"), "50% {braces} _x_ a|b");
    }

    /// A small seeded generator of wiki lines from the constructs above.
    fn generated(seed: u64, n: usize) -> String {
        const PARTS: &[&str] = &[
            "h2. ",
            "* ",
            "** ",
            "# ",
            "- ",
            "*b*",
            "{{m}}",
            "[t|https://e.example.com]",
            "[https://e.example.com]",
            "_i_",
            "{color:red}",
            "{color}",
            "|",
            "||",
            "x",
            " ",
            "\u{a0}",
            "`",
            "\\",
            "[",
            "]",
            "<",
            ">",
            "1. ",
            "*",
            "{code}",
            "\n",
            "\r\n",
            "{{",
            "}}",
            "+",
            "!",
            "~",
            "{panel}",
            "word",
            "2*3*4",
        ];
        let mut s = seed;
        let mut out = String::new();
        for _ in 0..n {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            out.push_str(PARTS[(s >> 33) as usize % PARTS.len()]);
        }
        out
    }

    #[test]
    fn generated_wiki_comes_back_as_it_was() {
        for seed in 0..3000 {
            let w = generated(seed, 2 + (seed as usize % 40));
            let md = to_markdown(&w);
            assert_eq!(
                to_wiki(&md),
                canonical(&w),
                "seed {seed}: {w:?}\nmd: {md:?}"
            );
        }
    }

    #[test]
    fn edited_markdown_keeps_its_characters() {
        for seed in 0..2000 {
            let md = generated(seed, 2 + (seed as usize % 30)).replace("||", "| |");
            let wiki = to_wiki(&md);
            // A list's number is markup (`1.` becomes `#`), not text.
            let text: String = md
                .lines()
                .map(|l| {
                    l.trim_start()
                        .trim_start_matches(|c: char| c.is_ascii_digit())
                })
                .collect();
            for c in text.chars().filter(|c| c.is_alphanumeric()) {
                assert!(
                    wiki.contains(c),
                    "seed {seed}: {c:?} lost from {md:?} → {wiki:?}"
                );
            }
        }
    }
}
