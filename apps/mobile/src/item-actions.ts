// What a row does: complete or reopen (with when a recurring one is next
// due), delete, open. Each change is undoable from the snackbar.
import { type Item, Status, deleteItem, setStatus } from "@todo-tui/client/portable";
import { useClients } from "@todo-tui/client/react";
import { dayLabel, dayOf } from "@todo-tui/present";
import { router } from "expo-router";
import { useCallback, useMemo } from "react";
import { tap } from "./haptics";
import { useSnack } from "./snack";

export const isClosed = (s: Status) => s === Status.DONE || s === Status.CANCELLED;

export interface ItemActions {
  toggle(item: Item): void;
  remove(item: Item): void;
  open(item: Item): void;
}

export function useItemActions(today: number): ItemActions {
  const clients = useClients();
  const snack = useSnack();

  const toggle = useCallback(
    async (item: Item) => {
      const reopen = isClosed(item.status);
      tap();
      try {
        const res = await setStatus(clients.items, item.id, reopen ? Status.NEXT : Status.DONE, { timeoutMs: 15_000 });
        const next = res.generated?.dueDate ? ` · next ${dayLabel(dayOf(res.generated.dueDate), today)}` : "";
        snack.done(`${reopen ? "Reopened" : "Done"}: ${item.title}${next}`, { ...(res.result && { result: res.result }), items: [res.item, res.generated] });
      } catch (err) {
        snack.fail(err);
      }
    },
    [clients, snack, today],
  );

  const remove = useCallback(
    async (item: Item) => {
      tap();
      try {
        const res = await deleteItem(clients.items, item.id, { timeoutMs: 15_000 });
        snack.done(`Deleted: ${item.title}`, { ...(res.result && { result: res.result }), items: [res.item] }, [], "trash");
      } catch (err) {
        snack.fail(err);
      }
    },
    [clients, snack],
  );

  const open = useCallback((item: Item) => router.push({ pathname: "/item/[id]", params: { id: item.id } }), []);

  return useMemo(() => ({ toggle: (i) => void toggle(i), remove: (i) => void remove(i), open }), [toggle, remove, open]);
}
