// A live list of Items: swipe right to complete or reopen, left to
// delete, tap ○ to complete, tap the row to open it.
import type { Item, ViewQuery } from "@todo-tui/client/portable";
import { useLiveStore, useView } from "@todo-tui/client/react";
import { type ReactElement, memo, useCallback, useState } from "react";
import { ActivityIndicator, FlatList, RefreshControl, StyleSheet, Text, View } from "react-native";
import { describeError } from "../errors";
import { type ItemActions, isClosed, useItemActions } from "../item-actions";
import { type Theme, useTheme } from "../theme";
import { ItemRow } from "./ItemRow";
import { SwipeRow } from "./SwipeRow";

export function ItemList({ query, today, empty, header }: { query: ViewQuery; today: number; empty: string; header?: ReactElement }) {
  const t = useTheme();
  const s = styles(t);
  const store = useLiveStore();
  const view = useView(query);
  const actions = useItemActions(today);
  const [refreshing, setRefreshing] = useState(false);
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
  const row = ({ item }: { item: Item }) => <SwipeItem item={item} today={today} actions={actions} />;
  return (
    <FlatList
      style={s.page}
      data={view.items}
      keyExtractor={(i) => i.id}
      renderItem={row}
      ItemSeparatorComponent={Separator}
      {...(header && { ListHeaderComponent: header })}
      refreshControl={<RefreshControl refreshing={refreshing} onRefresh={onRefresh} tintColor={t.dim} />}
      ListEmptyComponent={<Text style={[s.message, s.empty]}>{view.status === "error" ? describeError(view.error) : empty}</Text>}
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

/** One row, its swipe callbacks stable while its Item is unchanged. */
const SwipeItem = memo(function SwipeItem({ item, today, actions }: { item: Item; today: number; actions: ItemActions }) {
  const onRight = useCallback(() => actions.toggle(item), [actions, item]);
  const onLeft = useCallback(() => actions.remove(item), [actions, item]);
  return (
    <SwipeRow right={isClosed(item.status) ? REOPEN : DONE} left={DELETE} onRight={onRight} onLeft={onLeft}>
      <ItemRow item={item} today={today} onToggle={actions.toggle} onOpen={actions.open} />
    </SwipeRow>
  );
});

function Separator() {
  const s = styles(useTheme());
  return <View style={s.sep} />;
}

const styles = (t: Theme) =>
  StyleSheet.create({
    page: { flex: 1, backgroundColor: t.bg },
    center: { flex: 1, alignItems: "center", justifyContent: "center", backgroundColor: t.bg },
    message: { color: t.dim, textAlign: "center", fontSize: 15, paddingHorizontal: 24 },
    empty: { marginTop: 48 },
    end: { paddingBottom: 96 },
    sep: { height: StyleSheet.hairlineWidth, backgroundColor: t.border, marginLeft: 52 },
  });
