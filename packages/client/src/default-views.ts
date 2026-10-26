import type { MessageInitShape } from "@bufbuild/protobuf";
import { ItemField } from "./gen/todo/v1/field_pb.js";
import { type ClauseSchema, type OperandSchema, Operator, RelativeDate_Anchor, type SortKeySchema } from "./gen/todo/v1/filter_pb.js";
import { Status } from "./gen/todo/v1/item_pb.js";
import type { ViewSchema } from "./gen/todo/v1/view_pb.js";

type ClauseInit = MessageInitShape<typeof ClauseSchema>;
type OperandInit = MessageInitShape<typeof OperandSchema>;
type SortKeyInit = MessageInitShape<typeof SortKeySchema>;

const predicate = (target: ItemField, op: Operator, operands: OperandInit[] = []): ClauseInit => ({
  kind: { case: "predicate", value: { target, op, operands } },
});

const statuses = (...values: Status[]): ClauseInit =>
  predicate(
    ItemField.STATUS,
    Operator.IN,
    values.map((value) => ({ kind: { case: "value", value: { kind: { case: "status", value } } } })),
  );

const relative = (anchor: RelativeDate_Anchor): OperandInit => ({ kind: { case: "relativeDate", value: { anchor, offsetDays: 0 } } });

const isSet = (target: ItemField) => predicate(target, Operator.IS_SET);

const asc = (key: ItemField): SortKeyInit => ({ key });
const desc = (key: ItemField): SortKeyInit => ({ key, descending: true });

/**
 * The views seeded into every database (design 2.4), in tab order, as
 * `todo_core::view::defaults()` defines them: each with its seed key and
 * position, and no id, revision or timestamps. The implicit rules apply on
 * top of each filter, and the tie-break created_at, id follows each sort.
 */
export function defaultViews(): MessageInitShape<typeof ViewSchema>[] {
  const today = relative(RelativeDate_Anchor.TODAY);
  const rows: [string, string, MessageInitShape<typeof ViewSchema>["filter"], SortKeyInit[], boolean][] = [
    ["inbox", "Inbox", { allOf: [statuses(Status.INBOX)] }, [asc(ItemField.CREATED_AT)], true],
    [
      "today",
      "Today",
      {
        anyOf: [
          predicate(ItemField.DUE_DATE, Operator.LE, [today]),
          statuses(Status.IN_PROGRESS),
          { kind: { case: "group", value: { allOf: [isSet(ItemField.WAITING), predicate(ItemField.WAITING_FOLLOW_UP_ON, Operator.LE, [today])] } } },
          predicate(ItemField.FOCUS_WEEK, Operator.EQ, [relative(RelativeDate_Anchor.CURRENT_WEEK)]),
        ],
      },
      [desc(ItemField.IS_OVERDUE), desc(ItemField.EFFECTIVE_PRIORITY), asc(ItemField.DUE_DATE)],
      true,
    ],
    ["next", "Next", { allOf: [statuses(Status.NEXT, Status.IN_PROGRESS)] }, [desc(ItemField.EFFECTIVE_PRIORITY), asc(ItemField.DUE_DATE)], true],
    ["waiting", "Waiting", { allOf: [isSet(ItemField.WAITING)] }, [asc(ItemField.WAITING_SINCE)], true],
    ["someday", "Someday", { allOf: [statuses(Status.SOMEDAY)] }, [desc(ItemField.UPDATED_AT)], false],
    [
      "archive",
      "Archive",
      {
        // Done and cancelled Items before they are archived too, and
        // archived ones that are deferred.
        anyOf: [isSet(ItemField.ARCHIVED), statuses(Status.DONE, Status.CANCELLED)],
        includeArchived: true,
        includeDone: true,
        includeDeferred: true,
      },
      [desc(ItemField.UPDATED_AT)],
      false,
    ],
  ];
  return rows.map(([seed, name, filter, sort, showCount], position) => ({ name, filter, sort, showCount, position, seed }));
}
