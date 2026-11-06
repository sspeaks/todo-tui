import type { ReactNode } from "react";
import { KeyboardAvoidingView, Modal, Platform, Pressable, ScrollView, StyleSheet, Text, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { type Theme, useTheme } from "../../theme";

/** A bottom sheet: a title, Clear (when given) and Done, and its content. */
export function Sheet({
  title,
  visible,
  onClose,
  onClear,
  children,
}: {
  title: string;
  visible: boolean;
  onClose: () => void;
  onClear?: () => void;
  children: ReactNode;
}) {
  const t = useTheme();
  const s = styles(t);
  const insets = useSafeAreaInsets();
  return (
    <Modal visible={visible} transparent animationType="slide" onRequestClose={onClose}>
      <KeyboardAvoidingView style={s.fill} behavior={Platform.OS === "ios" ? "padding" : undefined}>
        <Pressable style={s.backdrop} onPress={onClose} accessibilityLabel="Close" />
        <View style={[s.sheet, { paddingBottom: insets.bottom + 12 }]}>
          <View style={s.header}>
            {onClear ? (
              <Pressable onPress={onClear} hitSlop={8}>
                <Text style={s.clear}>Clear</Text>
              </Pressable>
            ) : (
              <View style={s.spacer} />
            )}
            <Text style={s.title}>{title}</Text>
            <Pressable onPress={onClose} hitSlop={8}>
              <Text style={s.done}>Done</Text>
            </Pressable>
          </View>
          <ScrollView keyboardShouldPersistTaps="handled" contentContainerStyle={s.body}>
            {children}
          </ScrollView>
        </View>
      </KeyboardAvoidingView>
    </Modal>
  );
}

/** A tappable option in a sheet; `on` marks the chosen one. */
export function Option({ label, detail, on, onPress }: { label: string; detail?: string; on?: boolean; onPress: () => void }) {
  const s = styles(useTheme());
  return (
    <Pressable style={[s.option, on && s.optionOn]} onPress={onPress} accessibilityRole="button" accessibilityState={{ selected: !!on }}>
      <Text style={[s.optionText, on && s.optionTextOn]}>{label}</Text>
      {detail ? <Text style={[s.optionDetail, on && s.optionTextOn]}>{detail}</Text> : null}
    </Pressable>
  );
}

export const sheetStyles = styles;

function styles(t: Theme) {
  return StyleSheet.create({
    fill: { flex: 1, justifyContent: "flex-end" },
    backdrop: { position: "absolute", top: 0, right: 0, bottom: 0, left: 0, backgroundColor: "#0006" },
    sheet: { backgroundColor: t.bg, borderTopLeftRadius: 16, borderTopRightRadius: 16, maxHeight: "85%" },
    header: {
      flexDirection: "row",
      alignItems: "center",
      justifyContent: "space-between",
      paddingHorizontal: 16,
      paddingVertical: 14,
      borderBottomWidth: StyleSheet.hairlineWidth,
      borderBottomColor: t.border,
    },
    spacer: { width: 40 },
    title: { color: t.text, fontWeight: "600", fontSize: 16 },
    clear: { color: t.danger, fontSize: 15 },
    done: { color: t.accent, fontWeight: "600", fontSize: 15 },
    body: { padding: 16, gap: 8 },
    option: {
      flexDirection: "row",
      justifyContent: "space-between",
      alignItems: "center",
      paddingHorizontal: 14,
      paddingVertical: 12,
      borderRadius: 10,
      backgroundColor: t.surface,
    },
    optionOn: { backgroundColor: t.accent },
    optionText: { color: t.text, fontSize: 15 },
    optionDetail: { color: t.dim, fontSize: 14 },
    optionTextOn: { color: t.onAccent },
    row: { flexDirection: "row", flexWrap: "wrap", gap: 8 },
    pill: { paddingHorizontal: 12, paddingVertical: 8, borderRadius: 16, backgroundColor: t.surface },
    pillOn: { backgroundColor: t.accent },
    pillText: { color: t.text },
    input: { backgroundColor: t.surface, color: t.text, borderRadius: 8, paddingHorizontal: 12, paddingVertical: 10, fontSize: 15 },
    hint: { color: t.dim, fontSize: 13 },
    warn: { color: t.warn, fontSize: 13 },
  });
}
