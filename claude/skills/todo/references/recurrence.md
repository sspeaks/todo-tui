# Repeating items

A repeat rule lives on the item itself. A series is the chain of items it creates: there is no separate series object. Only one instance of a series is open at a time.

- **Completing or cancelling an instance** creates the next one in the same change. `todo_set_status` returns it as `next`. It has the same title, priority, labels and rule, and its due date is the next date on the schedule that's today or later (missed dates are skipped). Its status is `next` (or `someday`, if the completed one was someday), even when the first was captured to the inbox.
- **The next instance is deferred until its day**, so it stays out of every view until then. `todo_list` with `include_done` or `text` still finds it. Don't be surprised when it's missing from Today.
- **`@3d` / `@2w` count from completion**, not from the due date. Fixed rules (`@daily`, `@mon,thu`, `@15th`) keep to the calendar.
- **One undo reverts both**: `todo_undo` reopens the completed instance and removes the next one.
- **To skip an occurrence**, set it to `cancelled`. That creates the next instance just as done does.
- **To end a series**, clear the rule (`todo_set_repeat` with `""`) or delete the open instance. Undo or undelete brings it back. Clearing the rule on a completed instance isn't possible: rules can only be set on open items.
- **Changing the rule** changes only the open instance. Completed ones keep the rule they had.
- **Subtasks aren't copied** to the next instance, only the parent.
- **Done items are archived** automatically 7 days after they close. They show in the Archive view straight away.
- Repeat rules are **local only**: they're never pushed to Jira, even on a Jira-linked item, and later instances aren't linked to the issue.
