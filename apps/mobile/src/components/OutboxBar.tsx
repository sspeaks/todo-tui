import { Ionicons } from "@expo/vector-icons";
import { StyleSheet, Text, View } from "react-native";
import { useOutbox } from "../outbox";
import { type Theme, useTheme } from "../theme";

/** What was added offline and waits to be sent. */
export function OutboxBar() {
  const t = useTheme();
  const s = styles(t);
  const { pending } = useOutbox();
  if (pending.length === 0) return null;
  return (
    <View style={s.bar} accessibilityRole="summary">
      <Ionicons name="cloud-upload-outline" size={16} color={t.dim} />
      <Text style={s.text} numberOfLines={2}>
        {pending.length === 1 ? "1 waiting to send" : `${pending.length} waiting to send`}: {pending.map((p) => p.title).join(", ")}
      </Text>
    </View>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    bar: { flexDirection: "row", alignItems: "center", gap: 8, paddingHorizontal: 16, paddingVertical: 8, backgroundColor: t.surface },
    text: { flex: 1, color: t.dim, fontSize: 13 },
  });
