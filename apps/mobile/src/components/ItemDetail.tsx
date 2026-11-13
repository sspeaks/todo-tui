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
import { useState } from "react";
import { ActivityIndicator, Alert, KeyboardAvoidingView, Platform, Pressable, ScrollView, StyleSheet, Text, TextInput, View } from "react-native";
import { protoDate } from "../dates";
import { useKnown } from "../known";
import { withNotMine } from "../not-mine";
import { useSnack } from "../snack";
import { size, space, type Theme, useTheme } from "../theme";
import { useToday } from "../today";
import { CardGroup, CardRow } from "./Card";
import { ALL_STATUSES, StatusPicker } from "./pickers/StatusPicker";
import { PillRow } from "./quick/Pill";
import { type RepeatPreset, presetOf } from "./quick/presets";
import { QuickDate } from "./quick/QuickDate";
import { QuickLabels } from "./quick/QuickLabels";
import { QuickPriority } from "./quick/QuickPriority";
import { QuickRepeat } from "./quick/QuickRepeat";
import { QuickWaiting } from "./quick/QuickWaiting";
import { StatusMark } from "./StatusMark";

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
      const res = await withNotMine((once) =>
        updateItem(clients.items, { id: item.id, ...patch }, paths, {
          timeoutMs: 15_000,
          ...once,
          ...(opts.expectRevision && { expectedRevision: item.revision }),
        }),
      );
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
      const res = await withNotMine((once) => setStatus(clients.items, item.id, status, { timeoutMs: 15_000, ...once }));
      const label = ALL_STATUSES.find((x) => x.value === status)?.label ?? "";
      const next = res.generated?.dueDate ? ` · next ${dayLabel(dayOf(res.generated.dueDate), today)}` : "";
      snack.done(`${label}${next}`, { ...(res.result && { result: res.result }), items: [res.item, res.generated] });
    } catch (err) {
      snack.fail(err);
    }
  };

  const changeRepeat = async (choice: { rule: Parameters<typeof setRecurrence>[2] } | undefined) => {
    try {
      // A rule sets the due date, which pushes: the same question.
      const res = await withNotMine((once) => setRecurrence(clients.items, item.id, choice?.rule, { timeoutMs: 15_000, ...once }));
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
        <CardGroup>
          <View style={s.titleRow}>
            <View style={s.titleMark}>
              <StatusMark status={item.status} size={26} />
            </View>
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
          </View>
        </CardGroup>

        <CardGroup title="Status">
          <CardRow>
            <StatusPicker value={item.status} options={ALL_STATUSES} onChange={changeStatus} />
          </CardRow>
        </CardGroup>

        <CardGroup title="Priority">
          <CardRow>
            <PillRow>
              <QuickPriority
                value={item.priority === Priority.NONE ? undefined : item.priority}
                onChange={(pr) => void update(pr === undefined ? {} : { priority: pr }, ["priority"], pr === undefined ? "Priority cleared" : `Priority ${priorityName(pr)}`)}
              />
            </PillRow>
          </CardRow>
        </CardGroup>

        <CardGroup title="When">
          <CardRow icon="calendar-outline" label="Due" value={due === undefined ? undefined : dayLabel(due, today)} danger={item.isOverdue}>
            <PillRow>
              <QuickDate
                title="Due"
                value={due}
                today={today}
                onChange={(d) => void update(d === undefined ? {} : { dueDate: protoDate(d) }, ["due_date"], d === undefined ? "Due date cleared" : `Due ${dayLabel(d, today)}`)}
              />
            </PillRow>
          </CardRow>
          <CardRow icon="repeat-outline" label="Repeats" value={repeatsLine(item, today) ?? (isClosed ? "Reopen it to set a repeat" : undefined)}>
            {isClosed ? null : (
              <PillRow>
                <QuickRepeat
                  preset={presetOf(rule)}
                  custom={rule && !presetOf(rule) ? describeRule(rule) : undefined}
                  anchor={due}
                  current={rule ? ruleText(rule) : undefined}
                  onPreset={(preset) => void repeatPreset(preset)}
                  onChange={(c) => void changeRepeat(c)}
                />
              </PillRow>
            )}
          </CardRow>
          <CardRow icon="eye-off-outline" label="Hide until" value={defer === undefined ? undefined : dayLabel(defer, today)}>
            <PillRow>
              <QuickDate
                title="Hide until"
                value={defer}
                today={today}
                onChange={(d) =>
                  void update(d === undefined ? {} : { deferUntil: protoDate(d) }, ["defer_until"], d === undefined ? "Shown again" : `Hidden until ${dayLabel(d, today)}`)
                }
              />
            </PillRow>
          </CardRow>
        </CardGroup>

        <CardGroup title="People & labels">
          <CardRow icon="pricetag-outline" label="Labels">
            <PillRow>
              <QuickLabels
                value={item.labels}
                known={known.labels}
                onChange={(labels) => void update({ labels }, ["labels"], labels.length ? `Labels ${labels.map((l) => `#${l}`).join(" ")}` : "Labels cleared")}
              />
            </PillRow>
          </CardRow>
          <CardRow icon="hourglass-outline" label="Waiting on">
            <PillRow>
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
            </PillRow>
          </CardRow>
          {item.waiting ? (
            <CardRow icon="alarm-outline" label="Follow up" value={followUp === undefined ? undefined : dayLabel(followUp, today)} danger={followUp !== undefined && followUp <= today}>
              <PillRow>
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
              </PillRow>
            </CardRow>
          ) : null}
        </CardGroup>

        <CardGroup title="Notes">
          <CardRow>
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
          </CardRow>
        </CardGroup>

        <CardGroup title="Info">
          {item.parentTitle && item.parentId ? (
            <CardRow
              icon="git-branch-outline"
              label="Part of"
              value={item.parentTitle}
              valueColor={t.accent}
              onPress={() => router.push({ pathname: "/item/[id]", params: { id: item.parentId! } })}
            />
          ) : null}
          {item.subtasksTotal ? <CardRow icon="list-outline" label="Subtasks" value={`${item.subtasksDone}/${item.subtasksTotal} done`} /> : null}
          {item.createdAt ? (
            <CardRow
              icon="add-circle-outline"
              label="Created"
              value={`${localTime(item.createdAt)}${createdViaName(item.createdVia) ? ` · ${createdViaName(item.createdVia)}` : ""}`}
            />
          ) : null}
          {item.updatedAt ? <CardRow icon="time-outline" label="Changed" value={localTime(item.updatedAt)} /> : null}
        </CardGroup>

        <CardGroup>
          <CardRow label="Delete" onPress={confirmDelete} danger center />
        </CardGroup>
      </ScrollView>
    </KeyboardAvoidingView>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    page: { flex: 1, backgroundColor: t.grouped },
    center: { flex: 1, alignItems: "center", justifyContent: "center", backgroundColor: t.grouped },
    content: { padding: space.l, gap: space.l + 2, paddingBottom: 48 },
    titleRow: { flexDirection: "row", alignItems: "flex-start", gap: space.m, padding: 14 },
    titleMark: { paddingTop: 1 },
    title: { flex: 1, color: t.text, fontSize: size.title, fontWeight: "600", padding: 0 },
    closedTitle: { color: t.dim, textDecorationLine: "line-through" },
    notes: { color: t.text, minHeight: 110, fontSize: 15, lineHeight: 21, padding: 0 },
    notesActions: { flexDirection: "row", justifyContent: "flex-end", alignItems: "center", gap: space.l, marginTop: space.s },
    cancel: { color: t.dim, fontSize: 15 },
    save: { backgroundColor: t.accent, borderRadius: 8, paddingHorizontal: 14, paddingVertical: 8 },
    saveText: { ...t.monoBold, color: t.onAccent },
  });
