// One Item, every field the phone edits: title, status, priority, due,
// repeat, labels, waiting (and its follow-up), hide until, notes; and
// delete. Each change is its own op, applied at once and undoable from the
// snackbar.
import {
  type Item,
  type MessageInitShape,
  type ItemSchema,
  Priority,
  Status,
  deleteItem,
  parseRecurrence,
  setRecurrence,
  setStatus,
  updateItem,
} from "@todo-tui/client/portable";
import { useClients, useItem } from "@todo-tui/client/react";
import { createdViaName, dayLabel, dayOf, describeRule, localTime, personName, priorityName, repeatsLine, ruleText, shortId } from "@todo-tui/present";
import { Stack, router } from "expo-router";
import { type ReactNode, useState } from "react";
import { ActivityIndicator, Alert, KeyboardAvoidingView, Platform, Pressable, ScrollView, StyleSheet, Text, TextInput, View } from "react-native";
import { protoDate } from "../dates";
import { useKnown } from "../known";
import { useSnack } from "../snack";
import { type Theme, useTheme } from "../theme";
import { useToday } from "../today";
import { ALL_STATUSES, StatusPicker } from "./pickers/StatusPicker";
import { PillRow } from "./quick/Pill";
import { type RepeatPreset, presetOf } from "./quick/presets";
import { QuickDate } from "./quick/QuickDate";
import { QuickLabels } from "./quick/QuickLabels";
import { QuickPriority } from "./quick/QuickPriority";
import { QuickRepeat } from "./quick/QuickRepeat";
import { QuickWaiting } from "./quick/QuickWaiting";

type Patch = Omit<MessageInitShape<typeof ItemSchema>, "id" | "$typeName">;
// The plain-object form of Waiting (not the message, which needs $typeName).
type WaitingInit = Extract<NonNullable<Patch["waiting"]>, { $typeName?: undefined }>;

const closed = (s: Status) => s === Status.DONE || s === Status.CANCELLED;

export function ItemDetail({ id }: { id: string }) {
  const t = useTheme();
  const s = styles(t);
  const item = useItem(id, { fetch: true });
  if (!item)
    return (
      <View style={s.center}>
        <ActivityIndicator color={t.accent} />
      </View>
    );
  return <Loaded item={item} />;
}

function Loaded({ item }: { item: Item }) {
  const t = useTheme();
  const s = styles(t);
  const clients = useClients();
  const snack = useSnack();
  const today = useToday();
  const known = useKnown();
  /** The title and notes while being edited; undefined shows the Item's. */
  const [title, setTitle] = useState<string>();
  const [notes, setNotes] = useState<string>();

  const due = item.dueDate ? dayOf(item.dueDate) : undefined;
  const defer = item.deferUntil ? dayOf(item.deferUntil) : undefined;
  const waitingOn = personName(item.waiting?.on);
  const followUp = item.waiting?.followUpOn ? dayOf(item.waiting.followUpOn) : undefined;
  const rule = item.recurrence?.rule;
  const isClosed = closed(item.status);

  /** The Item's waiting with `change` applied, as an init: its fields copied, not the message spread. */
  const waitingWith = (change: Partial<WaitingInit>): WaitingInit => {
    const w = item.waiting;
    const base: WaitingInit = {
      ...(w?.on && { on: w.on }),
      ...(w?.reason && { reason: w.reason }),
      ...(w?.since && { since: w.since }),
      ...(w?.followUpOn && { followUpOn: w.followUpOn }),
    };
    return { ...base, ...change };
  };

  const update = async (patch: Patch, paths: string[], text: string, opts: { expectRevision?: boolean } = {}) => {
    try {
      const res = await updateItem(clients.items, { id: item.id, ...patch }, paths, {
        timeoutMs: 15_000,
        ...(opts.expectRevision && { expectedRevision: item.revision }),
      });
      snack.done(text, { ...(res.result && { result: res.result }), items: [res.item] });
      return true;
    } catch (err) {
      snack.fail(err);
      return false;
    }
  };

  const saveTitle = async () => {
    const next = title?.trim();
    if (next === undefined || next === item.title || !next) return setTitle(undefined);
    if (await update({ title: next }, ["title"], "Title changed", { expectRevision: true })) setTitle(undefined);
  };
  const saveNotes = async () => {
    if (notes === undefined || notes === item.description) return setNotes(undefined);
    if (await update({ description: notes }, ["description"], "Notes saved", { expectRevision: true })) setNotes(undefined);
  };

  const changeStatus = async (status: Status) => {
    if (status === item.status) return;
    try {
      const res = await setStatus(clients.items, item.id, status, { timeoutMs: 15_000 });
      const label = ALL_STATUSES.find((x) => x.value === status)?.label ?? "";
      const next = res.generated?.dueDate ? ` · next ${dayLabel(dayOf(res.generated.dueDate), today)}` : "";
      snack.done(`${label}${next}`, { ...(res.result && { result: res.result }), items: [res.item, res.generated] });
    } catch (err) {
      snack.fail(err);
    }
  };

  const changeRepeat = async (choice: { rule: Parameters<typeof setRecurrence>[2] } | undefined) => {
    try {
      const res = await setRecurrence(clients.items, item.id, choice?.rule, { timeoutMs: 15_000 });
      snack.done(choice ? "Repeat set" : "Repeat stopped", { ...(res.result && { result: res.result }), items: [res.item] });
    } catch (err) {
      snack.fail(err);
    }
  };

  /** A preset tapped: resolved against the due date, then set (undefined stops it). */
  const repeatPreset = async (preset: RepeatPreset | undefined) => {
    if (preset === undefined) return changeRepeat(undefined);
    try {
      const res = await parseRecurrence(clients.items, preset, { timeoutMs: 5_000, ...(due !== undefined && { anchor: protoDate(due) }) });
      if (!res.rule) return snack.show(res.warning || "Not a repeat rule");
      await changeRepeat({ rule: res.rule });
    } catch (err) {
      snack.fail(err);
    }
  };

  const confirmDelete = () =>
    Alert.alert("Delete this item?", item.subtasksTotal ? "Its subtasks are deleted with it. Undo brings them back." : "Undo brings it back.", [
      { text: "Cancel", style: "cancel" },
      {
        text: "Delete",
        style: "destructive",
        onPress: async () => {
          try {
            const res = await deleteItem(clients.items, item.id, { timeoutMs: 15_000 });
            snack.done(`Deleted: ${item.title}`, { ...(res.result && { result: res.result }), items: [res.item] }, [], "trash");
            router.back();
          } catch (err) {
            snack.fail(err);
          }
        },
      },
    ]);

  const notesDirty = notes !== undefined && notes !== item.description;

  return (
    <KeyboardAvoidingView style={s.page} behavior={Platform.OS === "ios" ? "padding" : undefined} keyboardVerticalOffset={100}>
      <Stack.Screen options={{ title: shortId(item.id) }} />
      <ScrollView contentContainerStyle={s.content} keyboardShouldPersistTaps="handled">
        <TextInput
          style={[s.title, isClosed && s.closedTitle]}
          value={title ?? item.title}
          onFocus={() => setTitle(item.title)}
          onChangeText={setTitle}
          onEndEditing={saveTitle}
          onSubmitEditing={saveTitle}
          submitBehavior="blurAndSubmit"
          returnKeyType="done"
          multiline
          accessibilityLabel="Title"
        />

        <StatusPicker value={item.status} options={ALL_STATUSES} onChange={changeStatus} />

        <Section label="Priority">
          <QuickPriority
            value={item.priority === Priority.NONE ? undefined : item.priority}
            onChange={(pr) => void update(pr === undefined ? {} : { priority: pr }, ["priority"], pr === undefined ? "Priority cleared" : `Priority ${priorityName(pr)}`)}
          />
        </Section>
        <Section label="Due" value={due === undefined ? undefined : dayLabel(due, today)} danger={item.isOverdue}>
          <QuickDate
            title="Due"
            value={due}
            today={today}
            onChange={(d) => void update(d === undefined ? {} : { dueDate: protoDate(d) }, ["due_date"], d === undefined ? "Due date cleared" : `Due ${dayLabel(d, today)}`)}
          />
        </Section>
        <Section label="Repeats" value={repeatsLine(item, today) ?? (isClosed ? "Reopen it to set a repeat" : undefined)}>
          {isClosed ? null : (
            <QuickRepeat
              preset={presetOf(rule)}
              custom={rule && !presetOf(rule) ? describeRule(rule) : undefined}
              anchor={due}
              current={rule ? ruleText(rule) : undefined}
              onPreset={(preset) => void repeatPreset(preset)}
              onChange={(c) => void changeRepeat(c)}
            />
          )}
        </Section>
        <Section label="Labels">
          <QuickLabels
            value={item.labels}
            known={known.labels}
            onChange={(labels) => void update({ labels }, ["labels"], labels.length ? `Labels ${labels.map((l) => `#${l}`).join(" ")}` : "Labels cleared")}
          />
        </Section>
        <Section label="Waiting on">
          <QuickWaiting
            value={waitingOn || undefined}
            known={known.names}
            onChange={(name) =>
              void update(
                name === undefined ? {} : { waiting: waitingWith({ on: { ref: { case: "freeText", value: name } } }) },
                ["waiting"],
                name === undefined ? "No longer waiting" : `Waiting on ${name}`,
              )
            }
          />
        </Section>
        {item.waiting ? (
          <Section label="Follow up" value={followUp === undefined ? undefined : dayLabel(followUp, today)} danger={followUp !== undefined && followUp <= today}>
            <QuickDate
              title="Follow up"
              value={followUp}
              today={today}
              onChange={(d) =>
                void update(
                  {
                    waiting:
                      d === undefined
                        ? (({ followUpOn: _, ...rest }) => rest)(waitingWith({}))
                        : waitingWith({ followUpOn: protoDate(d) }),
                  },
                  ["waiting.follow_up_on"],
                  d === undefined ? "Follow-up cleared" : `Follow up ${dayLabel(d, today)}`,
                )
              }
            />
          </Section>
        ) : null}
        <Section label="Hide until" value={defer === undefined ? undefined : dayLabel(defer, today)}>
          <QuickDate
            title="Hide until"
            value={defer}
            today={today}
            onChange={(d) =>
              void update(d === undefined ? {} : { deferUntil: protoDate(d) }, ["defer_until"], d === undefined ? "Shown again" : `Hidden until ${dayLabel(d, today)}`)
            }
          />
        </Section>

        <Text style={s.section}>Notes</Text>
        <TextInput
          style={s.notes}
          value={notes ?? item.description}
          onFocus={() => setNotes(item.description)}
          onChangeText={setNotes}
          placeholder="Add notes"
          placeholderTextColor={t.dim}
          multiline
          textAlignVertical="top"
          accessibilityLabel="Notes"
        />
        {notesDirty ? (
          <View style={s.notesActions}>
            <Pressable onPress={() => setNotes(undefined)} hitSlop={8}>
              <Text style={s.cancel}>Discard</Text>
            </Pressable>
            <Pressable style={s.save} onPress={saveNotes}>
              <Text style={s.saveText}>Save notes</Text>
            </Pressable>
          </View>
        ) : null}

        <View style={s.info}>
          {item.parentTitle && item.parentId ? (
            <Pressable onPress={() => router.push({ pathname: "/item/[id]", params: { id: item.parentId! } })}>
              <Text style={s.infoText}>
                Part of <Text style={s.link}>{item.parentTitle}</Text>
              </Text>
            </Pressable>
          ) : null}
          {item.subtasksTotal ? (
            <Text style={s.infoText}>
              Subtasks: {item.subtasksDone} of {item.subtasksTotal} done
            </Text>
          ) : null}
          {item.createdAt ? (
            <Text style={s.infoText}>
              Created {localTime(item.createdAt)}
              {createdViaName(item.createdVia) ? ` from ${createdViaName(item.createdVia)}` : ""}
            </Text>
          ) : null}
          {item.updatedAt ? <Text style={s.infoText}>Changed {localTime(item.updatedAt)}</Text> : null}
        </View>

        <Pressable style={s.delete} onPress={confirmDelete}>
          <Text style={s.deleteText}>Delete</Text>
        </Pressable>
      </ScrollView>

    </KeyboardAvoidingView>
  );
}

/** A field: its name and (when set) how it reads, then its one-tap choices. */
function Section({ label, value, danger, children }: { label: string; value?: string | undefined; danger?: boolean; children?: ReactNode }) {
  const s = styles(useTheme());
  return (
    <View style={s.field}>
      <View style={s.fieldHead}>
        <Text style={s.fieldLabel}>{label}</Text>
        {value ? (
          <Text style={[s.fieldValue, danger && s.danger]} numberOfLines={2}>
            {value}
          </Text>
        ) : null}
      </View>
      {children ? <PillRow>{children}</PillRow> : null}
    </View>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    page: { flex: 1, backgroundColor: t.bg },
    center: { flex: 1, alignItems: "center", justifyContent: "center", backgroundColor: t.bg },
    content: { padding: 16, gap: 14, paddingBottom: 48 },
    title: { color: t.text, fontSize: 22, fontWeight: "600", padding: 0 },
    closedTitle: { color: t.dim, textDecorationLine: "line-through" },
    field: { gap: 6 },
    fieldHead: { flexDirection: "row", justifyContent: "space-between", alignItems: "baseline", gap: 12 },
    fieldLabel: { color: t.dim, fontSize: 13, textTransform: "uppercase" },
    fieldValue: { color: t.text, fontSize: 14, flexShrink: 1, textAlign: "right" },
    danger: { color: t.danger, fontWeight: "600" },
    section: { color: t.dim, fontSize: 13, textTransform: "uppercase", marginTop: 4 },
    notes: { backgroundColor: t.surface, color: t.text, borderRadius: 12, padding: 12, minHeight: 120, fontSize: 15 },
    notesActions: { flexDirection: "row", justifyContent: "flex-end", alignItems: "center", gap: 16 },
    cancel: { color: t.dim, fontSize: 15 },
    save: { backgroundColor: t.accent, borderRadius: 8, paddingHorizontal: 14, paddingVertical: 8 },
    saveText: { color: t.onAccent, fontWeight: "600" },
    info: { gap: 4, marginTop: 4 },
    infoText: { color: t.dim, fontSize: 13 },
    link: { color: t.accent },
    delete: { alignSelf: "center", marginTop: 16, paddingHorizontal: 16, paddingVertical: 10 },
    deleteText: { color: t.danger, fontSize: 16 },
  });
