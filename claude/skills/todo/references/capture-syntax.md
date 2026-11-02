# Capture syntax

`todo_capture` and `todo add` parse the same grammar (the daemon's `ParseCapture`). A token is a whitespace-delimited word that starts with a sigil. Everything that isn't a token is the title.

## Tokens

| Token | Meaning | Examples |
|---|---|---|
| `+1` … `+5` | Priority, 1 = highest | `+2` |
| `^<date>` | Due date | `^fri`, `^tom`, `^+3d`, `^eow`, `^nov2`, `^2026-11-02` |
| `#<label>` | Label; repeat for more | `#home #errand` |
| `~<person>` | Waiting on someone (free text) | `~dana`, `~"the vendor"` |
| `>next`, `>someday` | Status instead of the default `inbox` | `>someday` |
| `@<rule>` | Repeat rule (below) | `@daily`, `@mon,thu`, `@15th`, `@2w` |
| `\` | Escapes a sigil so it stays in the title | `fix \#1 fan`, `email bob\@x.com` |

- A capture that is only tokens (`+2 #home`) keeps the whole text as its title, with a warning.
- A token that doesn't parse stays in the title, with a warning. The item is still created.
- An `@` or `#` inside a word (`bob@x.com`, `C#`) is title text.
- Text is at most 64 KiB.
- With the CLI, single-quote the text. Double quotes expand `$`, and unquoted `#`, `>`, `~` and `^` are taken by the shell.

## Dates

Dates are case-insensitive, resolve in local time, and are used by `^` in captures and by the date fields of `todo_update` (`due`, `defer_until`, `follow_up`):

| Date | Meaning |
|---|---|
| `today`, `tod`, `tomorrow`, `tom` | As named |
| `mon` … `sun` (3+ letters) | The next one strictly after today (`fri` on a Friday is next week) |
| `+3d`, `+2w`, `+1m` | Days, weeks or calendar months from today |
| `eow` | This week's Friday (next Friday on a weekend) |
| `eom` | The last day of this month |
| `2026-11-02` | That date |
| `nov2`, `nov02` | The next Nov 2 (this year if it hasn't passed) |

Phrases outside the grammar ("next Tuesday", "in two weeks", "Christmas") must be translated first: `tue`, `+2w`, `dec25`. When unsure what a vague phrase means ("end of next week"), compute the ISO date and use that.

## Repeat rules

| Rule | Repeats |
|---|---|
| `@daily` | Every day |
| `@weekdays` | Monday to Friday |
| `@mon,thu` | On those weekdays (each 3+ letters) |
| `@weekly` | On the weekday of the `^` date, else today's |
| `@monthly` | On the day of the month of the `^` date, else today's |
| `@15th`, `@1st`, `@22nd` | That day of every month, or the month's last day when it's shorter (`@31st`) |
| `@3d`, `@2w` | That long after completing the previous one (1 day to 10 years) |

Without a `^` date, the first scheduled date becomes the due date. The item shows its rule resolved: `^fri @weekly` comes back as `@fri`, and `@monthly` on the 15th as `@15th`.

"Every other week" is `@2w` (after completion), not a calendar rule. Fixed calendar schedules that aren't listed here (the first Monday of the month, every other Tuesday) aren't supported: say so, and offer the nearest rule.

## Examples

| User says | Capture text |
|---|---|
| remind me to call mom tomorrow | `Call mom ^tom` |
| high-priority: send Priya the Q4 deck by Friday, it's work | `Send Priya the Q4 deck +2 ^fri #work` |
| I'm waiting on the vendor for the quote | `Get quote ~"the vendor"` |
| someday maybe learn Rust macros | `Learn Rust macros >someday` |
| water the plants every Monday and Thursday | `Water the plants @mon,thu` |
| pay rent on the 1st of every month | `Pay rent @1st` |
| change the HVAC filter every 3 months | `Change the HVAC filter @13w` (or `^+3m @monthly`, if a fixed day is wanted) |
