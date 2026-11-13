# ADR-0011: People from the tracker, and who `~name` names

- Status: accepted
- Date: 2026-11-13
- Amends: design 3.5 (People), 2.3 (`~`), 2.5 (`w`), 2.9 (meeting attendees);
  P2's People milestone (M14)

## Context

todo-tui is a terminal todo manager. Its Rust daemon owns the data, and its clients
are thin: an Ink TUI, the `todo` CLI, an MCP server and a phone app. Its design is
[`docs/design.md`](../design.md). An Item has an assignee and a reporter, which are
the tracker's, and a `waiting.on`, which is the person's own: "who am I waiting on
for this". Design 3.5 planned for all three to point at Person records once the
tracker (Jira Data Center, ADR-0006) gave them, and kept them as free text until then.

So far a pull stored the assignee's display name as text, and `~dana` stored "dana".
Neither was tied to the other or to an address. The Teams meeting link (2.9, M8)
therefore found no attendees, unless the waiting-on text happened to be an email
address itself.

## Decision

**People come only from the tracker. `~name` links only when exactly one person
answers to it. The `w` prompt can also reach the tracker's directory.**

- **Made by sync.** Every user a pull, a reconciliation or a link reads becomes a
  Person. A Person has a display name, a username, emails, and one identity per
  tracker account (instance id and Data Center user key). The token's own user from
  `/myself` is a Person too. A pass finds each user by identity, so a renamed user
  is the same person:
  - the display name and username follow Jira;
  - a new address is added in front, and one Jira stops sending (hidden) is kept.
  
  Each change is a `TOPIC_PERSON` change, recorded before the Items that name the
  person. Migration `0012` marks every linked Item to be read again, so the next pass
  turns the free-text assignees and reporters into People. That is an ordinary sync
  change: no event history, and not an undo entry.
- **No local People.** A name typed for someone who is not in Jira (`~"the vendor"`)
  stays free text, as it always has. People are never made from a typed name, never
  deleted, and never merged; with one instance, no duplicates arise.
- **`~name` links on exactly one match.** It matches case-insensitively, in two
  tiers:
  - first, a username, an email address or a whole display name;
  - only if nothing matched that way, a display name's first word.
  
  If exactly one person answers in the first tier that names anyone, the text becomes
  that person. Two or more never guess: the text stays free text, and the draft
  (`ParseCapture`, `CaptureItem`) says how many matched (`waiting_matches`), so the
  preview reads "⧗alex (2 people match: kept as text)". A link is shown by name
  (`waiting_person`). The store matches inside the capture's own transaction, so a
  spool import gets the same answer. Existing free text is left alone: there is no
  backfill.
- **The directory, on request.** `PeopleService.SearchDirectory` asks Jira's
  `GET /rest/api/2/user/search?username=` (which matches a username, name or email
  address), keeps active users only, and leaves out those already known.
  `AddPerson` reads one by key and makes the Person.
  - In the TUI, a `w` tab that finds no one here asks the directory once. The preview
    says "(new, from Jira)", and enter adds the person and saves their id.
  - MCP's `todo_people` lists both but adds no one.
  
  Offline it fails `JIRA_UNAVAILABLE`, and the text can still be saved as free text.
- **Guarded.** An assignee, reporter or `waiting.on` given a person id must name a
  known person (`INVALID_ARGUMENT`, "names no known person"). Free text is still
  allowed only for `waiting.on`.
- **Meetings** invite each person's addresses: those of the waiting-on, the assignee
  and the reporter. A free-text waiting-on that is one address still counts.

## Consequences

- Every client names a Person by their display name, through `personName`. Until the
  People have loaded, or with a daemon from before People, a Person reads as
  "someone", never as their id. The client's `LivePeople` is always loaded, because
  any row may need a name.
- The `w` prompt and the phone's pills offer People first, then the free text already
  waited on. A typed name that one person answers to saves that person, as `~` does.
- `todo people [query] [--jira]` lists people from the command line.
- The export carries People before Items (record `person`), so a restore keeps every
  link.
- Left for later:
  - merging two People (a second instance could make one person twice);
  - editing a display name locally (Jira owns it);
  - `assignee:me` in query text;
  - linking old free text in bulk.
