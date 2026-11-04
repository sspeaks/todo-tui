import { useState } from "react";
import { Pressable, Text, TextInput, View } from "react-native";
import { cleanLabel } from "../../capture-draft";
import { useTheme } from "../../theme";
import { Sheet, sheetStyles } from "./Sheet";

/** Labels: the ones in use to tap on and off, and a field for a new one. */
export function LabelsSheet({
  visible,
  value,
  known,
  onChange,
  onClose,
}: {
  visible: boolean;
  value: readonly string[];
  known: readonly string[];
  onChange: (labels: string[]) => void;
  onClose: () => void;
}) {
  const t = useTheme();
  const s = sheetStyles(t);
  const [text, setText] = useState("");
  const all = [...new Set([...value, ...known])].sort((a, b) => a.localeCompare(b));
  const toggle = (l: string) => onChange(value.includes(l) ? value.filter((x) => x !== l) : [...value, l]);
  const add = () => {
    const l = cleanLabel(text);
    if (l && !value.includes(l)) onChange([...value, l]);
    setText("");
  };
  return (
    <Sheet title="Labels" visible={visible} onClose={onClose} {...(value.length > 0 && { onClear: () => onChange([]) })}>
      <TextInput
        style={s.input}
        value={text}
        onChangeText={setText}
        onSubmitEditing={add}
        submitBehavior="submit"
        placeholder="New label"
        placeholderTextColor={t.dim}
        autoCapitalize="none"
        autoCorrect={false}
        returnKeyType="done"
      />
      <View style={s.row}>
        {all.map((l) => {
          const on = value.includes(l);
          return (
            <Pressable key={l} style={[s.pill, on && s.pillOn]} onPress={() => toggle(l)} accessibilityState={{ selected: on }}>
              <Text style={[s.pillText, on && s.optionTextOn]}>#{l}</Text>
            </Pressable>
          );
        })}
      </View>
      {all.length === 0 ? <Text style={s.hint}>No labels yet. Type one above.</Text> : null}
    </Sheet>
  );
}
