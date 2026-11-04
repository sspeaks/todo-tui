import { type Item, Status, setStatus } from "@todo-tui/client/portable";
import { useClients, useConnection, useStatusCounts, useView, useViews, useLiveStore } from "@todo-tui/client/react";
import { dayLabel, dayOf, tabQuery } from "@todo-tui/present";
import { Stack, router } from "expo-router";
import { useCallback, useEffect, useState } from "react";
import { ActivityIndicator, Alert, FlatList, Pressable, RefreshControl, StyleSheet, Text, View } from "react-native";
import { SafeAreaView, useSafeAreaInsets } from "react-native-safe-area-context";
import { describeError, isUnauthenticated } from "../errors";
import { tap } from "../haptics";
import { useSnack } from "../snack";
import { type Theme, useTheme } from "../theme";
import { useToday } from "../today";
import { Banner } from "./Banner";
import { AddTodo } from "./AddTodo";
import { ItemRow } from "./ItemRow";
import { TabStrip } from "./TabStrip";

const closed = (s: Status) => s === Status.DONE || s === Status.CANCELLED;

/** The tabs, the selected tab's list, and the + button that adds; a row opens its item. */
export function Home({ onUnpair }: { onUnpair: (message?: string) => Promise<void> }) {
  const t = useTheme();
  const s = styles(t);
  const store = useLiveStore();
  const clients = useClients();
  const snack = useSnack();
  const views = useViews();
  const counts = useStatusCounts();
  const today = useToday();
  const [tab, setTab] = useState<string>();
  const [refreshing, setRefreshing] = useState(false);
  const [adding, setAdding] = useState(false);
  const insets = useSafeAreaInsets();

  const selected = tab !== undefined && views.views.some((v) => v.id === tab) ? tab : views.views[0]?.id;

  // A revoked token: the stream keeps failing UNAUTHENTICATED.
  const streamError = useConnection().error;
  useEffect(() => {
    if (isUnauthenticated(streamError)) void onUnpair(describeError(streamError));
  }, [streamError, onUnpair]);

  const toggle = useCallback(
    async (item: Item) => {
      const reopen = closed(item.status);
      tap();
      try {
        const res = await setStatus(clients.items, item.id, reopen ? Status.NEXT : Status.DONE, { timeoutMs: 15_000 });
        // A recurring Item: when the next one is due.
        const next = res.generated?.dueDate ? ` · next ${dayLabel(dayOf(res.generated.dueDate), today)}` : "";
        snack.done(`${reopen ? "Reopened" : "Done"}: ${item.title}${next}`, { ...(res.result && { result: res.result }), items: [res.item, res.generated] });
      } catch (err) {
        snack.fail(err);
      }
    },
    [clients, snack, today],
  );

  const openItem = useCallback((item: Item) => router.push({ pathname: "/item/[id]", params: { id: item.id } }), []);

  const onRefresh = useCallback(() => {
    setRefreshing(true);
    store.refresh();
    setTimeout(() => setRefreshing(false), 400);
  }, [store]);

  const confirmUnpair = () =>
    Alert.alert("Unpair this phone?", "You'll need to scan a new code from `todo remote pair`.", [
      { text: "Cancel", style: "cancel" },
      { text: "Unpair", style: "destructive", onPress: () => void onUnpair() },
    ]);

  return (
    <SafeAreaView style={s.page} edges={["bottom"]}>
      <Stack.Screen
        options={{
          headerRight: () => (
            <Pressable onPress={confirmUnpair} hitSlop={8}>
              <Text style={s.headerLink}>Unpair</Text>
            </Pressable>
          ),
        }}
      />
      <Banner />
      <TabStrip views={views.views} counts={counts.views} selected={selected} onSelect={setTab} />
      {selected ? (
        <List tab={selected} today={today} onToggle={toggle} onOpen={openItem} refreshing={refreshing} onRefresh={onRefresh} />
      ) : (
        <View style={s.center}>{views.status === "loading" ? <ActivityIndicator color={t.accent} /> : <Text style={s.message}>No views.</Text>}</View>
      )}
      <Pressable
        style={({ pressed }) => [s.fab, { bottom: insets.bottom + 20 }, pressed && s.fabPressed]}
        onPress={() => setAdding(true)}
        accessibilityRole="button"
        accessibilityLabel="New todo"
      >
        <Text style={s.fabPlus}>+</Text>
      </Pressable>
      <AddTodo visible={adding} today={today} onClose={() => setAdding(false)} />
    </SafeAreaView>
  );
}

function List({
  tab,
  today,
  onToggle,
  onOpen,
  refreshing,
  onRefresh,
}: {
  tab: string;
  today: number;
  onToggle: (item: Item) => void;
  onOpen: (item: Item) => void;
  refreshing: boolean;
  onRefresh: () => void;
}) {
  const t = useTheme();
  const s = styles(t);
  const view = useView(tabQuery(tab));
  if (view.status === "loading" && view.items.length === 0)
    return (
      <View style={s.center}>
        <ActivityIndicator color={t.accent} />
      </View>
    );
  return (
    <FlatList
      style={s.page}
      data={view.items}
      keyExtractor={(i) => i.id}
      renderItem={({ item }) => <ItemRow item={item} today={today} onToggle={onToggle} onOpen={onOpen} />}
      ItemSeparatorComponent={() => <View style={s.sep} />}
      refreshControl={<RefreshControl refreshing={refreshing} onRefresh={onRefresh} tintColor={t.dim} />}
      ListEmptyComponent={<Text style={[s.message, s.empty]}>{view.status === "error" ? describeError(view.error) : "Nothing here."}</Text>}
      keyboardShouldPersistTaps="handled"
      // Room to scroll the last row out from under the + button.
      contentContainerStyle={s.listEnd}
    />
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    page: { flex: 1, backgroundColor: t.bg },
    center: { flex: 1, alignItems: "center", justifyContent: "center", padding: 24, gap: 16, backgroundColor: t.bg },
    message: { color: t.dim, textAlign: "center", fontSize: 15 },
    empty: { marginTop: 48 },
    headerLink: { color: t.accent, fontSize: 15 },
    sep: { height: StyleSheet.hairlineWidth, backgroundColor: t.border, marginLeft: 52 },
    listEnd: { paddingBottom: 96 },
    fab: {
      position: "absolute",
      right: 20,
      width: 60,
      height: 60,
      borderRadius: 30,
      backgroundColor: t.accent,
      alignItems: "center",
      justifyContent: "center",
      shadowColor: "#000",
      shadowOpacity: 0.25,
      shadowRadius: 8,
      shadowOffset: { width: 0, height: 3 },
      elevation: 6,
    },
    fabPressed: { opacity: 0.8, transform: [{ scale: 0.96 }] },
    fabPlus: { color: t.onAccent, fontSize: 34, lineHeight: 38, fontWeight: "300" },
  });
