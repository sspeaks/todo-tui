// Hooks the components share: the controller and the UI state, the
// current tab's rows, the daemon's today.

import { type ViewQuery, type ViewSnapshot, useLiveStore, useStatusCounts, useView } from "@todo-tui/client";
import { createContext, useContext, useEffect, useMemo, useState, useSyncExternalStore } from "react";
import type { Controller } from "../controller/controller.js";
import { dayOf, localDay } from "../format/dates.js";
import { initialList, type ListState } from "../state/selection.js";
import type { TabId, UiState } from "../state/types.js";

export const ControllerContext = createContext<Controller | null>(null);

export function useController(): Controller {
  const controller = useContext(ControllerContext);
  if (!controller) throw new Error("useController: no ControllerContext above this component");
  return controller;
}

/** A slice of the UI state; `select` must return state as is (or a primitive), not build a new object. */
export function useUi<T>(select: (s: UiState) => T): T {
  const { ui } = useController();
  return useSyncExternalStore(ui.subscribe, () => select(ui.getState()));
}

/** A tab's rows, their ids, and its stored selection. */
export function useRows(tab: TabId): { snap: ViewSnapshot; ids: readonly string[]; list: ListState } {
  const snap = useView({ viewId: tab });
  const ids = useMemo(() => snap.items.map((i) => i.id), [snap.items]);
  const list = useUi((s) => s.lists.get(tab)) ?? initialList;
  return { snap, ids, list };
}

/** The daemon's today as a day number (the local date until the counts load): relative labels count from it. */
export function useToday(): number {
  const { today } = useStatusCounts();
  return today ? dayOf(today) : localDay(new Date());
}

const noSubscribe = () => () => {};
const noSnapshot = () => undefined;

/** useView for a query that may not exist yet. */
export function useOptionalView(query: ViewQuery | undefined): ViewSnapshot | undefined {
  const store = useLiveStore();
  const view = query === undefined ? undefined : store.view(query);
  return useSyncExternalStore(view?.subscribe ?? noSubscribe, view?.getSnapshot ?? noSnapshot);
}

/** `value`, once it has held still for `ms`. */
export function useDebounced<T>(value: T, ms: number): T {
  const [settled, setSettled] = useState(value);
  useEffect(() => {
    if (Object.is(settled, value)) return;
    const t = setTimeout(() => setSettled(value), ms);
    return () => clearTimeout(t);
  }, [value, settled, ms]);
  return settled;
}
