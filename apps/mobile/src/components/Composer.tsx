// Adding an item: a title, and buttons for the rest (due, priority,
// labels, waiting, status, repeat). The choices become capture tokens after
// the title (capture-draft.ts), so it is one capture, with ParseCapture's
// live preview of the whole text; typing tokens still works too. The
// request id is kept until the capture succeeds or the text changes, so a
// retry after an unknown outcome cannot capture twice.
import { type CaptureDraft, CreatedVia, captureItem, parseCapture, ulid } from "@todo-tui/client/portable";
import { useClients } from "@todo-tui/client/react";
import { Latest, captureSummary, describeRule, shortDay } from "@todo-tui/present";
import { router } from "expo-router";
import { useMemo, useRef, useState } from "react";
import { ActivityIndicator, Pressable, ScrollView, StyleSheet, Text, TextInput, View } from "react-native";
import { type CaptureChoices, NO_CHOICES, captureText, hasChoices } from "../capture-draft";
import { useKnown } from "../known";
import { useSnack } from "../snack";
import { type Theme, useTheme } from "../theme";
import { DateSheet } from "./pickers/DateSheet";
import { LabelsSheet } from "./pickers/LabelsSheet";
import { PrioritySheet, priorityText } from "./pickers/PrioritySheet";
import { RepeatSheet } from "./pickers/RepeatSheet";
import { Option, Sheet } from "./pickers/Sheet";
import { WaitingSheet } from "./pickers/WaitingSheet";

type Open = "due" | "priority" | "labels" | "waiting" | "status" | "repeat";

export function Composer({ today }: { today: number }) {
  const t = useTheme();
  const s = styles(t);
  const clients = useClients();
  const snack = useSnack();
  const known = useKnown();
  const [title, setTitle] = useState("");
  const [choices, setChoices] = useState<CaptureChoices>(NO_CHOICES);
  const [repeatText, setRepeatText] = useState<string>();
  const [open, setOpen] = useState<Open>();
  const [preview, setPreview] = useState<{ text: string; draft: CaptureDraft }>();
  const [busy, setBusy] = useState(false);
  const requestId = useRef<string | undefined>(undefined);

  const previews = useMemo(
    () =>
      new Latest<string, CaptureDraft | undefined>(
        async (input) => (await parseCapture(clients.items, input, { timeoutMs: 5_000 })).draft,
        (input, draft) => {
          if (draft) setPreview({ text: input, draft });
        },
      ),
    [clients],
  );

  /** Any change to what would be captured: a new request id, a new preview. */
  const update = (nextTitle: string, next: CaptureChoices) => {
    setTitle(nextTitle);
    setChoices(next);
    requestId.current = undefined;
    const text = captureText(nextTitle, next);
    if (nextTitle.trim()) previews.request(text);
  };
  const choose = (patch: Partial<CaptureChoices>) => update(title, { ...choices, ...patch });

  const text = captureText(title, choices);
  const submit = async () => {
    if (!title.trim() || busy) return;
    requestId.current ??= ulid();
    setBusy(true);
    try {
      const res = await captureItem(clients.items, text, { createdVia: CreatedVia.MOBILE, requestId: requestId.current, timeoutMs: 15_000 });
      const id = res.item?.id;
      snack.done(
        `Added: ${res.item?.title ?? title}`,
        { ...(res.result && { result: res.result }), items: [res.item] },
        id ? [{ label: "Details", onPress: () => router.push({ pathname: "/item/[id]", params: { id } }) }] : [],
      );
      setTitle("");
      setChoices(NO_CHOICES);
      setRepeatText(undefined);
      setPreview(undefined);
      requestId.current = undefined;
    } catch (err) {
      snack.fail(err);
    } finally {
      setBusy(false);
    }
  };

  const draft = preview?.text === text && title.trim() ? preview.draft : undefined;
  const warnings = draft?.tokens.filter((tok) => tok.warning).map((tok) => tok.warning) ?? [];
  const showPreview = draft && (draft.tokens.length > 0 || hasChoices(choices));

  const chips: { key: Open; label: string; value?: string }[] = [
    { key: "due", label: "📅 Due", ...(choices.due !== undefined && { value: `📅 ${shortDay(choices.due, today)}` }) },
    { key: "priority", label: "Priority", ...(priorityText(choices.priority) && { value: priorityText(choices.priority)! }) },
    { key: "labels", label: "# Labels", ...(choices.labels.length > 0 && { value: choices.labels.map((l) => `#${l}`).join(" ") }) },
    { key: "waiting", label: "⧗ Waiting on", ...(choices.waiting && { value: `⧗ ${choices.waiting}` }) },
    { key: "status", label: "Inbox", ...(choices.status && { value: choices.status === "next" ? "Next" : "Someday" }) },
    { key: "repeat", label: "🔁 Repeat", ...(repeatText && { value: `🔁 ${repeatText}` }) },
  ];

  return (
    <View style={s.wrap}>
      {showPreview ? (
        <Text style={s.preview} numberOfLines={2}>
          {captureSummary(draft, today)}
        </Text>
      ) : null}
      {warnings.map((w) => (
        <Text key={w} style={s.warn}>
          {w}
        </Text>
      ))}
      <ScrollView horizontal showsHorizontalScrollIndicator={false} contentContainerStyle={s.chips} keyboardShouldPersistTaps="always">
        {chips.map((c) => (
          <Pressable key={c.key} style={[s.chip, c.value !== undefined && s.chipOn]} onPress={() => setOpen(c.key)} accessibilityRole="button">
            <Text style={[s.chipText, c.value !== undefined && s.chipTextOn]} numberOfLines={1}>
              {c.value ?? c.label}
            </Text>
          </Pressable>
        ))}
      </ScrollView>
      <View style={s.bar}>
        <TextInput
          style={s.input}
          value={title}
          onChangeText={(next) => update(next, choices)}
          placeholder="Add a todo"
          placeholderTextColor={t.dim}
          returnKeyType="done"
          onSubmitEditing={submit}
          submitBehavior="submit"
          autoCorrect
        />
        <Pressable style={[s.add, (!title.trim() || busy) && s.disabled]} onPress={submit} disabled={!title.trim() || busy} accessibilityLabel="Add">
          {busy ? <ActivityIndicator color={t.onAccent} /> : <Text style={s.addText}>Add</Text>}
        </Pressable>
      </View>

      {open === "due" ? (
        <DateSheet title="Due" visible value={choices.due} today={today} onChange={(due) => choose({ due })} onClose={() => setOpen(undefined)} />
      ) : null}
      {open === "priority" ? (
        <PrioritySheet visible value={choices.priority} onChange={(priority) => choose({ priority })} onClose={() => setOpen(undefined)} />
      ) : null}
      {open === "labels" ? (
        <LabelsSheet visible value={choices.labels} known={known.labels} onChange={(labels) => choose({ labels })} onClose={() => setOpen(undefined)} />
      ) : null}
      {open === "waiting" ? (
        <WaitingSheet visible value={choices.waiting} known={known.names} onChange={(waiting) => choose({ waiting })} onClose={() => setOpen(undefined)} />
      ) : null}
      {open === "status" ? (
        <Sheet title="Status" visible onClose={() => setOpen(undefined)}>
          {([undefined, "next", "someday"] as const).map((st) => (
            <Option
              key={st ?? "inbox"}
              label={st === undefined ? "Inbox" : st === "next" ? "Next" : "Someday"}
              on={choices.status === st}
              onPress={() => {
                choose({ status: st });
                setOpen(undefined);
              }}
            />
          ))}
        </Sheet>
      ) : null}
      {open === "repeat" ? (
        <RepeatSheet
          visible
          value={choices.repeat}
          anchor={choices.due}
          onChange={(c) => {
            choose({ repeat: c?.text });
            setRepeatText(c ? describeRule(c.rule) : undefined);
          }}
          onClose={() => setOpen(undefined)}
        />
      ) : null}
    </View>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    wrap: { borderTopWidth: StyleSheet.hairlineWidth, borderTopColor: t.border, paddingVertical: 8, gap: 6, backgroundColor: t.bg },
    preview: { color: t.dim, fontSize: 13, paddingHorizontal: 12 },
    warn: { color: t.warn, fontSize: 13, paddingHorizontal: 12 },
    chips: { paddingHorizontal: 8, gap: 6 },
    chip: { paddingHorizontal: 10, paddingVertical: 6, borderRadius: 14, borderWidth: 1, borderColor: t.border, maxWidth: 220 },
    chipOn: { backgroundColor: t.accent, borderColor: t.accent },
    chipText: { color: t.dim, fontSize: 13 },
    chipTextOn: { color: t.onAccent },
    bar: { flexDirection: "row", gap: 8, paddingHorizontal: 8 },
    input: { flex: 1, backgroundColor: t.surface, color: t.text, borderRadius: 8, paddingHorizontal: 12, paddingVertical: 10, fontSize: 16 },
    add: { backgroundColor: t.accent, borderRadius: 8, paddingHorizontal: 16, justifyContent: "center" },
    addText: { color: t.onAccent, fontWeight: "600" },
    disabled: { opacity: 0.5 },
  });
