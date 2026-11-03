# ADR-0004: A view's filter is typed as query text, parsed by the daemon

- Status: accepted
- Date: 2026-11-02
- Amends: design 2.4 (views), 2.5 (the command line), 4.3 (ViewService), 4.8 (a
  "Query text" subsection)

## Context

todo-tui is a terminal todo manager. A Rust **daemon** owns the SQLite database and
serves a gRPC API; the clients (an Ink **TUI**, the `todo` CLI, an **MCP** server for
Claude Code) are thin. Its design is [`docs/design.md`](../design.md); section numbers
below are that document's. **M6** is the milestone that added config, view editing,
backups and export.

A **view** is a saved filter and sort, shown as a tab in the TUI (Inbox, Today, Next,
…; design 2.4). M6 lets you make and edit views with `:view` on the TUI's command line
(typed after `:`, as in vim).

The filter language (design 4.8, `proto/todo/v1/filter.proto`) is a typed tree:
`all_of` (every clause holds) and `any_of` (one does), nested groups, and predicates
of a **target** (an Item field such as `due_date`), an operator (`IN`, `EQ`, `LT`,
`IS_SET`, `CONTAINS_ANY` on sets such as labels, `MATCHES` for full-text word search,
…) and operands, including relative dates such as "today plus 3 days". It has no
NOT: every operator has its complement (`EQ` / `NE`, `IS_SET` / `IS_UNSET`) instead,
which keeps the SQL it compiles to simple.

Nothing turned text into a filter; every filter was built in code. The design named
`:view new …` without saying how its filter is written.

## Decision

**A filter is written as query text**, parsed in `todo-core` (`query.rs`), the crate
of domain rules with no I/O, so it is tested without a database, and served by the
daemon's `ViewService.ParseQuery` RPC. The TUI prompt previews each keystroke through it:
the match count, or the first error with its span coloured in the line.

```
is:next,in_progress #work priority>=high due_date<=today+3d any:(has:waiting title~review) sort:-priority count:off
```

- Terms are separated by whitespace and ANDed. Field names are `ItemField` paths
  (`due_date`, `waiting.on`). `field:a,b` is IN (CONTAINS_ANY on sets), `field:=v`
  EQ, `!=` `<` `<=` `>` `>=` as written, `:all=` CONTAINS_ALL, `~` MATCHES. `is:`,
  `has:`, `no:`, `#label`, and bare words or `"quoted text"` (title MATCHES) are
  shorthand: `is:s` is `status:s`, `has:f` and `no:f` are IS_SET and IS_UNSET,
  `#work` is `labels:work`. `-` negates only `:` (`-f:v` is `f!=v`) and `has:`/`no:`,
  the terms whose complement exists: a filter has no NOT to negate anything else.
  `any:( … )` and `( … )` are groups. `sort:k,-k` sorts (`-` descending),
  `include:done` lifts a rule that hides done (or archived, deferred, deleted) Items,
  and `count:off` hides the tab's count. Dates are ISO or `today`, `week_start`,
  `week_end` with `+3d` / `-2w`; enum values are their snake_case names.
- `print_query` writes any filter, sort and count flag back as canonical text, which
  parses to the same query. The daemon prints it whenever it sends a view, as
  `View.query` (it is not stored, so it cannot drift from the filter), so
  `:view edit` starts from it and no client needs a printer.
- Errors are spans in code points (as capture tokens' already are, so the TUI
  converts both alike), collected in one pass and returned in the response, not as
  a failed call: an error in what you are typing is the normal case of a preview,
  not a failure. The TUI shows the first; the rest are there for any client. An
  error the filter's own validator finds (an operator a field does not take) points
  at the term that produced it.

## Alternatives considered

- **The capture tokens** (quick-add's `#label +2 ^fri`, design 2.3). Familiar, but
  they only AND simple terms: no "before today", no "any of", no sort. The Today
  view's own filter cannot be written.
- **A document in `$EDITOR`** (TOML or JSON of the filter). Complete, but slow for a
  quick view, and with no live count.
- **A parser in TypeScript too.** The TUI could preview without a round trip, but
  two parsers drift. The round trip to a local daemon is about a millisecond, so
  the grammar is the daemon's alone. FakeDaemon, the TypeScript stand-in for the
  daemon that client tests run against, answers `ParseQuery` only for text a test
  gives it.

## Consequences

- One grammar for every client: the TUI, MCP and any later client send text and get
  the same answer.
- The printer is part of the contract: a filter the API can build but text cannot
  (a group with both `all_of` and `any_of`) prints as one matching the same Items.
  A test round-trips every target, operator and operand kind.
- Query text is a user-facing syntax now, and changes to it follow the API's
  additive rule (design 4.3): new terms may come; existing text keeps its meaning.
