# ADR-0009: Jira descriptions as Markdown, with the rest of the wiki kept

- Status: accepted
- Date: 2026-11-11
- Amends: design 3.2 (the description of a linked Item), 5.6 (description
  safety), 5.2 (`to_remote_text` / `from_remote_text`), open question 3; follows
  ADR-0006 and ADR-0007

## Context

todo-tui is a terminal todo manager whose Rust daemon owns the data and whose
clients (an Ink TUI, the `todo` CLI, an MCP server, a phone app) are thin; its
design is [`docs/design.md`](../design.md). ADR-0007 made a Jira-linked Item's
title, priority, labels, due date and status two-way, and left the **description**
read-only: Jira Data Center stores it as **wiki markup** (`h2.`, `*bold*`,
`{{code}}`, `[text|url]`, `{code}` blocks, tables, `{color}`), while an Item's
description is Markdown everywhere else. Until now the daemon copied the wiki text
into the Item verbatim.

Editing it here needs a converter both ways that never loses what a person or a
teammate wrote. A sample of 40 recent issues of the project it serves used:
headings, bold, italic, monospace, links (`[text|url]`, `[url]`), bullets (`*`,
`**`, `-`, some with a leading space), numbered lists (`#`), `{code}` and
`{code:java}`, tables (`||head||`, `|cell|`), `{color}` spans, `\r\n` line ends and
non-breaking spaces.

Design 5.6 had planned for loss: a `description_lossy` flag, a warning before an
edit, and a refusal to push a lossy description without a confirmation flag.

## Decision

**Convert what Markdown can hold; keep the rest as it was, never drop it.**

- `todo_jira::wiki::to_markdown` and `to_wiki`, hand-written, line by line. They
  convert headings, bullets and numbered lists (one kind per list, nesting by
  depth), `*bold*`, `{{monospace}}`, `[text|url]` and `[url]` links, `{code}` and
  `{code:lang}` blocks, and tables of one heading row and plain cells (GFM pipes).
- **Kept as it is**: anything else. Text that reads as text stays text (`_italic_`
  and `{color:#888}…{color}` are left in the line as typed); a line that would not
  come back as it was becomes a `wiki:` code span (`` `wiki:…` ``), and a block that
  would not (a `{panel}`, a code block closed on its last line, a ragged table)
  becomes a ```` ```wiki ```` fenced block. `to_wiki` copies both back verbatim, and
  a person can still edit the text inside them.
- **Exact by construction**: `to_markdown` checks each line and block it converts
  against `to_wiki`, and keeps it raw when they differ; the whole is checked again
  at the end. So `to_wiki(to_markdown(w))` is `canonical(w)` (`\n` line ends, no
  trailing spaces or blank lines, a list marker at its line's start) for every
  description, and an unedited one pushes nothing new.
- **No lossy flag.** Nothing is ever lost, so there is nothing to warn about:
  `description_lossy` stays false and the confirmation it would have asked for is
  not built. A person's own Markdown converts by its constructs, and every
  character without a meaning in Markdown is kept as typed.

## Alternatives considered

- **Warn, then allow** (design 5.6 as written): convert what can be, flag the
  rest lossy, ask before an edit, drop the formatting on push. Simpler output, but
  a routine edit of a sub-task with a `{color}` footer would drop its colour.
- **Refuse lossy descriptions**: never lose anything, but leave many tickets
  uneditable here.
- **A Markdown library** (pulldown-cmark, in the build already through
  prost-build): it parses Markdown well, but it re-renders it in its own
  spelling, which breaks the exact round trip, and it does not know wiki markup.

## Consequences

- A description edited here may hold `wiki:` spans and ```` ```wiki ```` blocks;
  clients show them as code, and the MCP tools and the Claude skill say to keep
  them as they are.
- Existing linked Items hold wiki text; they are read again by id once (migration
  `0009`) and take the Markdown then.
- Jira's renderer is lenient, and a corner the converter reads differently is kept
  raw rather than misconverted. A push reads back through the same converter, so a
  difference shows as a Suggestion, never a silent overwrite.
