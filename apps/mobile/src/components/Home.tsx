import { type Item, Status, setStatus, undo } from "@todo-tui/client/portable";
import { useClients, useConnection, useLiveStore, useStatusCounts, useView, useViews } from "@todo-tui/client/react";
import { dayOf, localDay, tabQuery } from "@todo-tui/present";
import { Stack } from "expo-router";
import { useCallback, useEffect, useState } from "react";
import { ActivityIndicator, Alert, FlatList, KeyboardAvoidingView, Platform, Pressable, RefreshControl, StyleSheet, Text, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { describeError, isUnauthenticated } from "../errors";
import { type Theme, useTheme } from "../theme";
import { Banner } from "./Banner";
import { ItemRow } from "./ItemRow";
import { QuickAdd } from "./QuickAdd";
import { type Snack, Snackbar } from "./Snackbar";
import { TabStrip } from "./TabStrip";

/** The tabs, the selected tab's list, quick-add, and complete / undo. */
export function Home({ onUnpair }: { onUnpair: (message?: string) => Promise<void> }) {
  const t = useTheme();
  const s = styles(t);
  const store = useLiveStore();
  const clients = useClients();
  const views = useViews();
  const counts = useStatusCounts();
  const [tab, setTab] = useState<string>();
  const [snack, setSnack] = useState<Snack>();
  const [refreshing, setRefreshing] = useState(false);

  const today = counts.today ? dayOf(counts.today) : localDay(new Date());
  const selected = tab !== undefined && views.views.some((v) => v.id === tab) ? tab : views.views[0]?.id;

  // A revoked token: the stream keeps failing UNAUTHENTICATED.
  const streamError = useConnection().error;
  useEffect(() => {
    if (isUnauthenticated(streamError)) void onUnpair(describeError(streamError));
  }, [streamError, onUnpair]);

  const show = useCallback((text: string, opts: { opId?: string; error?: boolean } = {}) => setSnack({ key: Date.now(), text, ...opts }), []);
  const dismiss = useCallback(() => setSnack(undefined), []);

  const toggle = useCallback(
    async (item: Item) => {
      const reopen = item.status === Status.DONE || item.status === Status.CANCELLED;
      try {
        const res = await setStatus(clients.items, item.id, reopen ? Status.NEXT : Status.DONE, { timeoutMs: 15_000 });
        const changed = [res.item, res.generated].filter((i): i is Item => i !== undefined);
        if (res.result) store.applyItems(changed, res.result.revision);
        const next = res.generated?.dueDate ? ` · next ^${res.generated.dueDate.month}/${res.generated.dueDate.day}` : "";
        show(`${reopen ? "Reopened" : "Done"}: ${item.title}${next}`, { opId: res.result?.opId });
      } catch (err) {
        show(describeError(err), { error: true });
      }
    },
    [clients, store, show],
  );

  const onUndo = useCallback(
    async (opId: string) => {
      try {
        const res = await undo(clients.items, { expectedOpId: opId, timeoutMs: 15_000 });
        if (res.result) store.applyItems(res.items, res.result.revision);
        show(res.entry ? `Undid: ${res.entry.description}` : "Nothing to undo");
      } catch (err) {
        show(describeError(err), { error: true });
      }
    },
    [clients, store, show],
  );

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
      <KeyboardAvoidingView style={s.page} behavior={Platform.OS === "ios" ? "padding" : undefined} keyboardVerticalOffset={100}>
        {selected ? (
          <List tab={selected} today={today} onToggle={toggle} refreshing={refreshing} onRefresh={onRefresh} />
        ) : (
          <View style={s.center}>{views.status === "loading" ? <ActivityIndicator color={t.accent} /> : <Text style={s.message}>No views.</Text>}</View>
        )}
        <Snackbar snack={snack} onUndo={onUndo} onDismiss={dismiss} />
        <QuickAdd today={today} onCaptured={(title, opId) => show(`Added: ${title}`, opId ? { opId } : {})} />
      </KeyboardAvoidingView>
    </SafeAreaView>
  );
}

function List({
  tab,
  today,
  onToggle,
  refreshing,
  onRefresh,
}: {
  tab: string;
  today: number;
  onToggle: (item: Item) => void;
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
      renderItem={({ item }) => <ItemRow item={item} today={today} onToggle={onToggle} />}
      ItemSeparatorComponent={() => <View style={s.sep} />}
      refreshControl={<RefreshControl refreshing={refreshing} onRefresh={onRefresh} tintColor={t.dim} />}
      ListEmptyComponent={<Text style={[s.message, s.empty]}>{view.status === "error" ? describeError(view.error) : "Nothing here."}</Text>}
      keyboardShouldPersistTaps="handled"
    />
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    page: { flex: 1, backgroundColor: t.bg },
    center: { flex: 1, alignItems: "center", justifyContent: "center", padding: 24, gap: 16, backgroundColor: t.bg },
    message: { color: t.dim, textAlign: "center", fontSize: 15 },
    empty: { marginTop: 48 },
    button: { backgroundColor: t.accent, borderRadius: 8, paddingHorizontal: 20, paddingVertical: 12 },
    buttonText: { color: t.onAccent, fontWeight: "600" },
    headerLink: { color: t.accent, fontSize: 15 },
    sep: { height: StyleSheet.hairlineWidth, backgroundColor: t.border, marginLeft: 52 },
  });
