import { ItemField, Operator, Status, type ViewQuery } from "@todo-tui/client";

/** The Inbox view (design 2.4): status inbox, oldest first. */
export const inboxQuery: ViewQuery = {
  filter: {
    allOf: [
      {
        kind: {
          case: "predicate",
          value: {
            target: ItemField.STATUS,
            op: Operator.IN,
            operands: [{ kind: { case: "value", value: { kind: { case: "status", value: Status.INBOX } } } }],
          },
        },
      },
    ],
  },
  sort: [{ key: ItemField.CREATED_AT }, { key: ItemField.ID }],
};
