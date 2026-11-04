import { useState } from "react";
import { Pressable, Text, TextInput, View } from "react-native";
import { useTheme } from "../../theme";
import { Sheet, sheetStyles } from "./Sheet";

/** Who it waits on: a name, typed or one already in use. */
export function WaitingSheet({
  visible,
  value,
  known,
  onChange,
  onClose,
}: {
  visible: boolean;
  value: string | undefined;
  known: readonly string[];
  onChange: (name: string | undefined) => void;
  onClose: () => void;
}) {
  const t = useTheme();
  const s = sheetStyles(t);
  const [text, setText] = useState(value ?? "");
  const finish = (name: string | undefined) => {
    const clean = name?.trim();
    onChange(clean ? clean : undefined);
    onClose();
  };
  const matches = known.filter((n) => n !== value && n.toLowerCase().includes(text.trim().toLowerCase()));
  return (
    <Sheet title="Waiting on" visible={visible} onClose={() => finish(text)} {...(value !== undefined && { onClear: () => finish(undefined) })}>
      <TextInput
        style={s.input}
        value={text}
        onChangeText={setText}
        onSubmitEditing={() => finish(text)}
        submitBehavior="submit"
        placeholder="Name, e.g. dana or the vendor"
        placeholderTextColor={t.dim}
        autoCorrect={false}
        returnKeyType="done"
        autoFocus
      />
      <View style={s.row}>
        {matches.map((n) => (
          <Pressable key={n} style={s.pill} onPress={() => finish(n)}>
            <Text style={s.pillText}>⧗{n}</Text>
          </Pressable>
        ))}
      </View>
    </Sheet>
  );
}
