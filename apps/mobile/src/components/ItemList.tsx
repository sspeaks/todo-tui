// A live list of Items: swipe right to complete or reopen, left to
// delete, tap ○ to complete, tap the row to open it. A row completed with
// a tap stays a beat, checked, before it goes; a row that comes or goes
// (one completed, deleted or added) fades in or out and the rows close up.
import { type Item, ItemSchema, Status, type ViewQuery, clone } from "@todo-tui/client/portable";
import { useLiveStore, usePeople, useView } from "@todo-tui/client/react";
import type { People } from "@todo-tui/present";
import { type ReactElement, memo, useCallback, useMemo, useState } from "react";
import { ActivityIndicator, FlatList, LayoutAnimation, RefreshControl, StyleSheet, View } from "react-native";
import { describeError } from "../errors";
import { type ItemActions, isClosed, useItemActions } from "../item-actions";
import { type Theme, useTheme } from "../theme";
import { EmptyState, type EmptyIcon } from "./EmptyState";
import { ItemRow, ROW_INSET } from "./ItemRow";
import { SwipeRow } from "./SwipeRow";

/** `empty` (with `emptyIcon` and `emptyHint`) is what shows with no rows. */
export function ItemList({
  query,
  today,
  empty,
  emptyIcon = "file-tray-outline",
  emptyHint,
  header,
}: {
  query: ViewQuery;
  today: number;
  empty: string;
  emptyIcon?: EmptyIcon;
  emptyHint?: string;
  header?: ReactElement;
}) {
  const t = useTheme();
  const s = styles(t);
  const store = useLiveStore();
  const view = useView(query);
  const actions = useItemActions(today);
  const people = usePeople().people;
  const [refreshing, setRefreshing] = useState(false);
  const [kept, setKept] = useState<ReadonlyMap<string, Kept>>(NONE_KEPT);
  const rows = useMemo(() => withKept(view.items, kept), [view.items, kept]);
  useRowAnimation(rows, view.status !== "loading");

  /** A tap on the mark: completing, the row stays (checked) until LINGER_MS after the tap, even once the view drops it. */
  const toggle = useCallback(
    async (item: Item) => {
      if (isClosed(item.status)) return actions.toggle(item);
      const started = Date.now();
      const index = view.items.findIndex((i) => i.id === item.id);
      // Kept as done: the row may already have shown the Item's own change (done) before the view dropped it.
      const done = clone(ItemSchema, item);
      done.status = Status.DONE;
      setKept((m) => new Map(m).set(item.id, { item: done, index: Math.max(0, index) }));
      const changed = await actions.toggle(item);
      const drop = () =>
        setKept((m) => {
          if (!m.has(item.id)) return m;
          const next = new Map(m);
          next.delete(item.id);
          return next;
        });
      if (!changed) drop();
      else setTimeout(drop, Math.max(0, LINGER_MS - (Date.now() - started)));
      return changed;
    },
    [actions, view.items],
  );
  const onRefresh = useCallback(() => {
    setRefreshing(true);
    store.refresh();
    setTimeout(() => setRefreshing(false), 400);
  }, [store]);

  if (view.status === "loading" && view.items.length === 0)
    return (
      <View style={s.center}>
        {header}
        <ActivityIndicator color={t.accent} />
      </View>
    );
  const row = ({ item }: { item: Item }) => <SwipeItem item={item} today={today} people={people} actions={actions} onToggle={toggle} />;
  return (
    <FlatList
      style={s.page}
      data={rows}
      keyExtractor={(i) => i.id}
      renderItem={row}
      // People that arrive after the rows name them (design 3.5).
      extraData={people}
      ItemSeparatorComponent={Separator}
      {...(header && { ListHeaderComponent: header })}
      refreshControl={<RefreshControl refreshing={refreshing} onRefresh={onRefresh} tintColor={t.dim} />}
      ListEmptyComponent={
        view.status === "error" ? <EmptyState icon="warning-outline" text={describeError(view.error)} tone="warn" /> : <EmptyState icon={emptyIcon} text={empty} hint={emptyHint} />
      }
      keyboardShouldPersistTaps="handled"
      keyboardDismissMode="on-drag"
      // Room to scroll the last row out from under the + button.
      contentContainerStyle={s.end}
    />
  );
}

const DELETE = { label: "Delete", icon: "trash-outline" } as const;
const DONE = { label: "Done", icon: "checkmark" } as const;
const REOPEN = { label: "Reopen", icon: "refresh" } as const;

/**
 * One row, its swipe callbacks stable while its Item is unchanged. A swipe
 * slides the row away itself, so it needs no lingering: it toggles
 * directly. A tap on the mark goes through `onToggle`, which lingers.
 */
const SwipeItem = memo(function SwipeItem({
  item,
  today,
  people,
  actions,
  onToggle,
}: {
  item: Item;
  today: number;
  people: People;
  actions: ItemActions;
  onToggle: (item: Item) => Promise<boolean>;
}) {
  const onRight = useCallback(() => actions.toggle(item), [actions, item]);
  const onLeft = useCallback(() => actions.remove(item), [actions, item]);
  return (
    <SwipeRow right={isClosed(item.status) ? REOPEN : DONE} left={DELETE} onRight={onRight} onLeft={onLeft} settled={`${item.status}:${item.revision}`}>
      <ItemRow item={item} today={today} people={people} onToggle={onToggle} onOpen={actions.open} />
    </SwipeRow>
  );
});

/** How long a row completed with a tap stays, checked, after the tap. */
export const LINGER_MS = 900;

/** A row kept after its view dropped it: the Item as it was tapped, and where it was. */
interface Kept {
  item: Item;
  index: number;
}
const NONE_KEPT: ReadonlyMap<string, Kept> = new Map();

/** The view's rows with the kept ones it no longer has put back where they were. */
function withKept(items: readonly Item[], kept: ReadonlyMap<string, Kept>): readonly Item[] {
  if (!kept.size) return items;
  const ids = new Set(items.map((i) => i.id));
  const rows = [...items];
  for (const [id, k] of [...kept].sort(([, a], [, b]) => a.index - b.index)) if (!ids.has(id)) rows.splice(Math.min(k.index, rows.length), 0, k.item);
  return rows;
}

/** At most this many rows coming or going animate; more (a refresh, a new search) just redraw. */
const ANIMATED_CHANGES = 3;

const idsOf = (key: string): string[] => (key ? key.split("\n") : []);

/**
 * Animates the next layout when a few rows came or went. LayoutAnimation
 * must be configured before the commit that changes the layout, and the
 * rows come from the live store, so it is configured as the new rows
 * render (React's state from the previous render: the rows it showed).
 * The first rows a view shows (`ready` turning true) do not animate; a tab
 * switch remounts the list (Home keys it by tab), so neither does that.
 */
function useRowAnimation(items: readonly Item[], ready: boolean): void {
  const key = ready ? items.map((i) => i.id).join("\n") : undefined;
  const [shown, setShown] = useState(key);
  if (shown === key) return;
  setShown(key);
  if (shown === undefined || key === undefined) return;
  const before = new Set(idsOf(shown));
  const now = new Set(idsOf(key));
  const changes = [...now].filter((id) => !before.has(id)).length + [...before].filter((id) => !now.has(id)).length;
  if (changes > 0 && changes <= ANIMATED_CHANGES) LayoutAnimation.configureNext(LayoutAnimation.create(220, "easeInEaseOut", "opacity"));
}

function Separator() {
  const s = styles(useTheme());
  return <View style={s.sep} />;
}

const styles = (t: Theme) =>
  StyleSheet.create({
    page: { flex: 1, backgroundColor: t.bg },
    center: { flex: 1, alignItems: "center", justifyContent: "center", backgroundColor: t.bg },
    end: { paddingBottom: 96 },
    sep: { height: StyleSheet.hairlineWidth, backgroundColor: t.border, marginLeft: ROW_INSET },
  });
