// Search everything, done and archived too: titles, labels, the remote key
// and notes, as `:search` does in the TUI, the most recently changed first.
import { matchText, searchTab, tabQuery } from "@todo-tui/present";
import { useFocusEffect } from "expo-router";
import { type ComponentRef, useCallback, useRef, useState } from "react";
import { StyleSheet, Text, TextInput, View } from "react-native";
import { type Theme, useTheme } from "../theme";
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
      {q === undefined ? (
        <Text style={s.hint}>Every item, done and archived ones too. Words match from their start: “dent” finds “dentist”.</Text>
      ) : (
        <ItemList query={tabQuery(searchTab(q))} today={today} empty={`Nothing matches "${q}".`} />
      )}
    </View>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    page: { flex: 1, backgroundColor: t.bg },
    input: { margin: 12, backgroundColor: t.surface, color: t.text, borderRadius: 10, paddingHorizontal: 12, paddingVertical: 10, fontSize: 16 },
    hint: { color: t.dim, paddingHorizontal: 16, fontSize: 14, lineHeight: 20 },
  });
