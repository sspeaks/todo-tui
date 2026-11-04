import { Ionicons } from "@expo/vector-icons";
import { useStatusCounts, useViews } from "@todo-tui/client/react";
import { tabQuery } from "@todo-tui/present";
import { useState } from "react";
import { ActivityIndicator, Pressable, StyleSheet, Text, View } from "react-native";
import { type Theme, useTheme } from "../theme";
import { useToday } from "../today";
import { AddTodo } from "./AddTodo";
import { Banner } from "./Banner";
import { ItemList } from "./ItemList";
import { OutboxBar } from "./OutboxBar";
import { TabStrip } from "./TabStrip";

/** The views as tabs, the selected one's list, and the + button that adds. */
export function Home() {
  const t = useTheme();
  const s = styles(t);
  const views = useViews();
  const counts = useStatusCounts();
  const today = useToday();
  const [tab, setTab] = useState<string>();
  const [adding, setAdding] = useState(false);
  const selected = tab !== undefined && views.views.some((v) => v.id === tab) ? tab : views.views[0]?.id;

  return (
    <View style={s.page}>
      <Banner />
      <TabStrip views={views.views} counts={counts.views} selected={selected} onSelect={setTab} />
      <OutboxBar />
      {selected ? (
        <ItemList query={tabQuery(selected)} today={today} empty="Nothing here." />
      ) : (
        <View style={s.center}>{views.status === "loading" ? <ActivityIndicator color={t.accent} /> : <Text style={s.message}>No views yet.</Text>}</View>
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

const styles = (t: Theme) =>
  StyleSheet.create({
    page: { flex: 1, backgroundColor: t.bg },
    center: { flex: 1, alignItems: "center", justifyContent: "center", padding: 24, backgroundColor: t.bg },
    message: { color: t.dim, textAlign: "center", fontSize: 15 },
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
