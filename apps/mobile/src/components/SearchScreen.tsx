// Search everything, done and archived too: titles, labels, the remote key
// and notes, as `:search` does in the TUI, the most recently changed first.
import { Ionicons } from "@expo/vector-icons";
import { matchText, searchTab, tabQuery } from "@todo-tui/present";
import { useFocusEffect } from "expo-router";
import { type ComponentRef, useCallback, useRef, useState } from "react";
import { StyleSheet, Text, TextInput, View } from "react-native";
import { space, type Theme, useTheme } from "../theme";
import { useToday } from "../today";
import { ItemList } from "./ItemList";

export function SearchScreen() {
  const t = useTheme();
  const s = styles(t);
  const today = useToday();
  const [text, setText] = useState("");
  const input = useRef<ComponentRef<typeof TextInput>>(null);
  // Choosing the tab is asking to search: the keyboard comes up.
  useFocusEffect(useCallback(() => void input.current?.focus(), []));
  const q = matchText(text);
  return (
    <View style={s.page}>
      <View style={s.field}>
        <Ionicons name="search-outline" size={18} color={t.dim} />
        <TextInput
          ref={input}
          style={s.input}
          value={text}
          onChangeText={setText}
          placeholder="Search titles, labels and notes"
          placeholderTextColor={t.dim}
          autoCorrect={false}
          returnKeyType="search"
          clearButtonMode="while-editing"
          accessibilityLabel="Search"
        />
      </View>
      {q === undefined ? (
        <View style={s.hintCard}>
          <Text style={s.hint}>Every item, done and archived ones too: titles, labels, the Jira key and notes.</Text>
          <Text style={s.hint}>
            Words match from their start: <Text style={s.example}>dent</Text> finds <Text style={s.example}>dentist</Text>.
          </Text>
        </View>
      ) : (
        <ItemList query={tabQuery(searchTab(q))} today={today} empty={`Nothing matches "${q}".`} emptyIcon="search-outline" emptyHint="Done and archived items count too" />
      )}
    </View>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    page: { flex: 1, backgroundColor: t.bg },
    // A chip's fill and a hairline: plainly the place to type, on paper and at night.
    field: {
      flexDirection: "row",
      alignItems: "center",
      gap: space.s,
      margin: space.m,
      backgroundColor: t.chip,
      borderWidth: StyleSheet.hairlineWidth,
      borderColor: t.border,
      borderRadius: 10,
      paddingHorizontal: space.m,
    },
    input: { flex: 1, color: t.text, paddingVertical: 10, fontSize: 16 },
    hintCard: { marginHorizontal: space.m, padding: 14, gap: 6, borderRadius: 10, borderWidth: StyleSheet.hairlineWidth, borderColor: t.border },
    hint: { color: t.dim, fontSize: 14, lineHeight: 20 },
    example: { ...t.mono, color: t.accent },
  });
