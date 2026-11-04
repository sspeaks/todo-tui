// Adding an item: a title, and above it one row per field of one-tap
// choices (due, priority, status, repeat, labels, waiting on), each
// wrapping so every choice is in sight. Tapping a chosen
// pill again clears it. Only a calendar date, a new label, a new name or
// an unusual repeat opens a sheet. The choices become capture tokens after
// the title (capture-draft.ts), so it is one capture with ParseCapture's
// live preview and one Undo, and typed tokens still work. The request id
// is kept until the capture succeeds or the text changes, so a retry after
// an unknown outcome cannot capture twice.
import { type CaptureDraft, CreatedVia, captureItem, parseCapture, ulid } from "@todo-tui/client/portable";
import { useClients } from "@todo-tui/client/react";
import { Latest, captureSummary, describeRule } from "@todo-tui/present";
import { router } from "expo-router";
import { useMemo, useRef, useState } from "react";
import { ActivityIndicator, Pressable, ScrollView, StyleSheet, Text, TextInput, View } from "react-native";
import { type CaptureChoices, NO_CHOICES, captureText, hasChoices } from "../capture-draft";
import { useKnown } from "../known";
import { useSnack } from "../snack";
import { type Theme, useTheme } from "../theme";
import { Pill, PillGroup } from "./quick/Pill";
import { REPEAT_PRESETS, type RepeatPreset } from "./quick/presets";
import { QuickDate } from "./quick/QuickDate";
import { QuickLabels } from "./quick/QuickLabels";
import { QuickPriority } from "./quick/QuickPriority";
import { QuickRepeat } from "./quick/QuickRepeat";
import { QuickWaiting } from "./quick/QuickWaiting";

export function Composer({ today }: { today: number }) {
  const t = useTheme();
  const s = styles(t);
  const clients = useClients();
  const snack = useSnack();
  const known = useKnown();
  const [title, setTitle] = useState("");
  const [choices, setChoices] = useState<CaptureChoices>(NO_CHOICES);
  /** How a repeat from the sheet reads ("every Mon, Thu"); presets need none. */
  const [customRepeat, setCustomRepeat] = useState<string>();
  const [focused, setFocused] = useState(false);
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
    if (nextTitle.trim()) previews.request(captureText(nextTitle, next));
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
      setCustomRepeat(undefined);
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
  const showChoices = focused || title.length > 0 || hasChoices(choices);
  const preset = REPEAT_PRESETS.find((p) => p.text === choices.repeat)?.text;

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
      {showChoices ? (
        <ScrollView style={s.choices} contentContainerStyle={s.choicesContent} keyboardShouldPersistTaps="always">
          <PillGroup label="Due">
            <QuickDate title="Due" value={choices.due} today={today} onChange={(due) => choose({ due })} />
          </PillGroup>
          <PillGroup label="Priority">
            <QuickPriority value={choices.priority} onChange={(priority) => choose({ priority })} />
          </PillGroup>
          <PillGroup label="Status">
            <Pill label="Next" on={choices.status === "next"} onPress={() => choose({ status: choices.status === "next" ? undefined : "next" })} />
            <Pill
              label="Someday"
              on={choices.status === "someday"}
              onPress={() => choose({ status: choices.status === "someday" ? undefined : "someday" })}
            />
          </PillGroup>
          <PillGroup label="Repeat">
            <QuickRepeat
              preset={preset as RepeatPreset | undefined}
              custom={preset ? undefined : customRepeat}
              anchor={choices.due}
              current={choices.repeat}
              onPreset={(p) => {
                setCustomRepeat(undefined);
                choose({ repeat: p });
              }}
              onChange={(c) => {
                setCustomRepeat(c ? describeRule(c.rule) : undefined);
                choose({ repeat: c?.text });
              }}
            />
          </PillGroup>
          <PillGroup label="Labels">
            <QuickLabels value={choices.labels} known={known.labels} onChange={(labels) => choose({ labels })} />
          </PillGroup>
          <PillGroup label="Waiting on">
            <QuickWaiting value={choices.waiting} known={known.names} onChange={(waiting) => choose({ waiting })} />
          </PillGroup>
        </ScrollView>
      ) : null}
      <View style={s.bar}>
        <TextInput
          style={s.input}
          value={title}
          onChangeText={(next) => update(next, choices)}
          onFocus={() => setFocused(true)}
          onBlur={() => setFocused(false)}
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
    </View>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    // Shrinks only after the list above it has: the choices then scroll.
    wrap: { flexShrink: 1, borderTopWidth: StyleSheet.hairlineWidth, borderTopColor: t.border, paddingVertical: 8, gap: 6, backgroundColor: t.bg },
    choices: { flexGrow: 0, flexShrink: 1 },
    choicesContent: { gap: 8, paddingVertical: 2 },
    preview: { color: t.dim, fontSize: 13, paddingHorizontal: 12 },
    warn: { color: t.warn, fontSize: 13, paddingHorizontal: 12 },
    bar: { flexDirection: "row", gap: 8, paddingHorizontal: 8 },
    input: { flex: 1, backgroundColor: t.surface, color: t.text, borderRadius: 8, paddingHorizontal: 12, paddingVertical: 10, fontSize: 16 },
    add: { backgroundColor: t.accent, borderRadius: 8, paddingHorizontal: 16, justifyContent: "center" },
    addText: { color: t.onAccent, fontWeight: "600" },
    disabled: { opacity: 0.5 },
  });
