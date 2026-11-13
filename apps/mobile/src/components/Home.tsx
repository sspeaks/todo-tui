import { Ionicons } from "@expo/vector-icons";
import { useConnection, useStatusCounts, useViews } from "@todo-tui/client/react";
import { tabQuery } from "@todo-tui/present";
import { useNavigation } from "expo-router";
import { useEffect, useLayoutEffect, useState } from "react";
import { ActivityIndicator, Pressable, StyleSheet, Text, View } from "react-native";
import { size, space, type Theme, useTheme } from "../theme";
import { useToday } from "../today";
import { AddTodo } from "./AddTodo";
import { Banner, useBannerText } from "./Banner";
import { EmptyState } from "./EmptyState";
import { ItemList } from "./ItemList";
import { OutboxBar } from "./OutboxBar";
import { TabStrip } from "./TabStrip";

/** How long the list kept from last time shows before the header says it is updating: a quick answer shows no cue. */
export const UPDATING_AFTER_MS = 400;

/**
 * The views as tabs, the selected one's list (its name the header's title),
 * and the + button that adds. While the list is the one kept from last time
 * and the computer has not answered yet, the header says "updating…" (the
 * banner says why instead, once there is one).
 */
export function Home() {
  const t = useTheme();
  const s = styles(t);
  const views = useViews();
  const counts = useStatusCounts();
  const today = useToday();
  const [tab, setTab] = useState<string>();
  const [adding, setAdding] = useState(false);
  const selected = tab !== undefined && views.views.some((v) => v.id === tab) ? tab : views.views[0]?.id;
  const name = views.views.find((v) => v.id === selected)?.name;
  const cached = useConnection().cached;
  const banner = useBannerText();
  const updating = cached && banner === undefined;
  const navigation = useNavigation();
  useLayoutEffect(
    () => navigation.setOptions({ title: name ?? "todo", headerRight: updating ? () => <Updating /> : undefined }),
    [navigation, name, updating],
  );

  return (
    <View style={s.page}>
      <Banner />
      <TabStrip views={views.views} counts={counts.views} selected={selected} onSelect={setTab} />
      <OutboxBar />
      {selected ? (
        <ItemList key={selected} query={tabQuery(selected)} today={today} empty="Nothing here." emptyIcon="checkmark-done-outline" emptyHint="Pull to refresh · + adds one" />
      ) : views.status === "loading" ? (
        <View style={s.center}>
          <ActivityIndicator color={t.accent} />
        </View>
      ) : (
        <EmptyState icon="albums-outline" text="No views yet." hint="Make one on your computer: :view new <name>" />
      )}
      <Pressable
        style={({ pressed }) => [s.fab, pressed && s.fabPressed]}
        onPress={() => setAdding(true)}
        accessibilityRole="button"
        accessibilityLabel="New todo"
      >
        <Ionicons name="add" size={34} color={t.onAccent} />
      </Pressable>
      <AddTodo visible={adding} today={today} onClose={() => setAdding(false)} />
    </View>
  );
}

/** "updating…", dim, once UPDATING_AFTER_MS have passed. */
function Updating() {
  const s = styles(useTheme());
  const [shown, setShown] = useState(false);
  useEffect(() => {
    const timer = setTimeout(() => setShown(true), UPDATING_AFTER_MS);
    return () => clearTimeout(timer);
  }, []);
  if (!shown) return null;
  return (
    <Text style={s.updating} accessibilityLiveRegion="polite">
      updating…
    </Text>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    page: { flex: 1, backgroundColor: t.bg },
    updating: { ...t.mono, color: t.dim, fontSize: size.caption, marginRight: space.l },
    center: { flex: 1, alignItems: "center", justifyContent: "center", padding: 24, backgroundColor: t.bg },
    fab: {
      position: "absolute",
      right: 20,
      // Above the tab bar, which the screen ends at.
      bottom: 20,
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
  });
