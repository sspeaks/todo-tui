import {
  type Item,
  ItemField,
  Operator,
  type Relation,
  RelationKind,
  type ViewQuery,
  useItem,
} from "@todo-tui/client";
import { type DOMElement, Box, Text, useBoxMetrics } from "ink";
import { type ReactNode, useEffect, useRef } from "react";
import { dayOf, localTime, longDay, relativeDays, shortDay } from "../format/dates.js";
import { createdViaName, personName, priorityName, shortId, statusGlyph, statusName } from "../format/item.js";
import { sanitize, singleLine } from "../format/text.js";
import { useController, useDebounced, useOptionalView, useToday, useUi } from "./hooks.js";

/** Selection changes settle this long before the parent, subtasks and relations are fetched (holding j fetches nothing). */
export const DETAIL_FETCH_MS = 80;

const LABEL = 10;

/** Direct children, whatever their status (Decision 4). */
const subtasksOf = (id: string): ViewQuery => ({
  filter: {
    allOf: [
      {
        kind: {
          case: "predicate",
          value: { target: ItemField.PARENT_ID, op: Operator.EQ, operands: [{ kind: { case: "value", value: { kind: { case: "itemId", value: id } } } }] },
        },
      },
    ],
    includeDone: true,
    includeArchived: true,
    includeDeferred: true,
  },
});

function Field({ label, children, dim }: { label: string; children: ReactNode; dim?: boolean }) {
  return (
    <Box>
      <Box width={LABEL} flexShrink={0}>
        <Text dimColor>{label}</Text>
      </Box>
      <Box flexGrow={1} flexShrink={1}>
        <Text wrap="wrap" dimColor={dim ?? false}>
          {children}
        </Text>
      </Box>
    </Box>
  );
}

function Heading({ children }: { children: ReactNode }) {
  return (
    <Box marginTop={1}>
      <Text bold>{children}</Text>
    </Box>
  );
}

const minutes = (d: { seconds: bigint } | undefined) => {
  if (!d) return "";
  const m = Math.round(Number(d.seconds) / 60);
  return m >= 60 ? `${Math.floor(m / 60)}h${m % 60 ? ` ${m % 60}m` : ""}` : `${m}m`;
};

/** "Blocks", "Blocked by", …, read from `item`'s end of the relation. */
function relationWord(kind: RelationKind, outgoing: boolean): string {
  switch (kind) {
    case RelationKind.BLOCKS:
      return outgoing ? "Blocks" : "Blocked by";
    case RelationKind.DUPLICATES:
      return outgoing ? "Duplicates" : "Duplicated by";
    default:
      return "Related to";
  }
}

/** One relation; its other end comes from the store, or is fetched once `fetch` (the selection has settled). */
function RelationLine({ item, relation, fetch }: { item: Item; relation: Relation; fetch: boolean }) {
  const outgoing = relation.fromId === item.id;
  const otherId = outgoing ? relation.toId : relation.fromId;
  const other = useItem(otherId, { fetch });
  const deleted = other?.deletedAt !== undefined;
  return (
    <Text wrap="truncate-end" dimColor={!other || deleted}>
      {"  "}
      {relationWord(relation.kind, outgoing)}{" "}
      {other ? `${statusGlyph(other.status)} ${singleLine(other.title)}` : shortId(otherId)}
      {deleted ? " (deleted)" : ""}
    </Text>
  );
}

/**
 * The read-only detail of one Item (Decision 4), scrolled when focused.
 * Its fields show at once, and so do a parent or a relation's other end
 * that the store holds; what it lacks, and the subtasks, are fetched once
 * the selection settles.
 */
export function DetailPane({ id, width, height, focused }: { id: string | undefined; width: number; height: number; focused: boolean }) {
  const controller = useController();
  const item = useItem(id);
  const settledId = useDebounced(id, DETAIL_FETCH_MS);
  const settled = settledId === id && item !== undefined;
  const parent = useItem(item?.parentId, { fetch: settled });
  const subtasks = useOptionalView(settled && item ? subtasksOf(item.id) : undefined);
  const scroll = useUi((s) => (s.detail.id === id ? s.detail.scroll : 0));
  const today = useToday();
  const content = useRef<DOMElement>(null);
  const { height: lines, hasMeasured } = useBoxMetrics(content);
  useEffect(() => {
    if (hasMeasured) controller.setDetailLines(id, lines);
  }, [controller, id, lines, hasMeasured]);

  if (!item) {
    return (
      <Box width={width} height={height} paddingLeft={1}>
        <Text dimColor>{id === undefined ? "Nothing selected" : "Loading…"}</Text>
      </Box>
    );
  }
  const day = (d: { year: number; month: number; day: number } | undefined, overdue = false) => {
    if (!d) return "—";
    const n = dayOf(d);
    return `${longDay(n)} (${relativeDays(n, today)}${overdue ? ", overdue" : ""})`;
  };
  const waiting = item.waiting;
  const override = item.localPriorityOverride;
  const kids = subtasks?.items ?? [];
  return (
    <Box width={width} height={height} overflow="hidden" flexDirection="column" paddingLeft={1} contentOffsetY={scroll}>
      <Box ref={content} flexDirection="column" flexShrink={0}>
        <Text bold wrap="wrap" color={focused ? "cyan" : undefined}>
          {singleLine(item.title)}
        </Text>
        <Field label="Status">
          {statusGlyph(item.status)} {item.remoteStatusName ? singleLine(item.remoteStatusName) : statusName(item.status)}
        </Field>
        <Field label="Priority">
          {priorityName(item.effectivePriority)}
          {override !== undefined ? ` (local override; field ${priorityName(item.priority)})` : ""}
        </Field>
        <Field label="Due" dim={!item.dueDate}>
          <Text color={item.isOverdue ? "red" : undefined}>{day(item.dueDate, item.isOverdue)}</Text>
        </Field>
        <Field label="Defer" dim={!item.deferUntil}>
          {day(item.deferUntil)}
        </Field>
        <Field label="Waiting" dim={!waiting}>
          {waiting
            ? [
                personName(waiting.on) || "someone",
                waiting.since ? `since ${shortDay(dayOf(waiting.since), today)} (${Math.max(0, today - dayOf(waiting.since))}d)` : "",
                waiting.followUpOn ? `follow up ${shortDay(dayOf(waiting.followUpOn), today)}` : "",
                singleLine(waiting.reason),
              ]
                .filter(Boolean)
                .join(" · ")
            : "—"}
        </Field>
        <Field label="Labels" dim={!item.labels.length}>
          {item.labels.length ? item.labels.map((l) => `#${singleLine(l)}`).join(" ") : "—"}
        </Field>
        {item.parentId !== undefined ? (
          <Field label="Parent" dim={parent?.deletedAt !== undefined}>
            {parent ? `${statusGlyph(parent.status)} ${singleLine(parent.title)}${parent.deletedAt ? " (deleted)" : ""}` : shortId(item.parentId)}
          </Field>
        ) : null}
        {item.itemType && item.itemType !== "task" ? <Field label="Type">{singleLine(item.itemType)}</Field> : null}
        {item.storyPoints !== undefined ? <Field label="Points">{String(item.storyPoints)}</Field> : null}
        {item.originalEstimate || item.remainingEstimate ? (
          <Field label="Estimate">
            {[minutes(item.originalEstimate), item.remainingEstimate ? `${minutes(item.remainingEstimate)} left` : ""].filter(Boolean).join(" · ")}
          </Field>
        ) : null}
        {item.archived ? <Field label="Archived">{item.archived.at ? localTime(item.archived.at) : "yes"}</Field> : null}
        {item.deletedAt ? <Field label="Deleted">{localTime(item.deletedAt)}</Field> : null}
        <Field label="Created">
          {item.createdAt ? localTime(item.createdAt) : "—"}
          {createdViaName(item.createdVia) ? ` via ${createdViaName(item.createdVia)}` : ""}
        </Field>
        <Field label="Updated">{item.updatedAt ? localTime(item.updatedAt) : "—"}</Field>
        <Field label="Id">{shortId(item.id)}</Field>
        {kids.length ? (
          <>
            <Heading>Subtasks ({kids.length})</Heading>
            {kids.map((k) => (
              <Text key={k.id} wrap="truncate-end" dimColor={k.archived !== undefined}>
                {"  "}
                {statusGlyph(k.status)} {singleLine(k.title)}
              </Text>
            ))}
          </>
        ) : null}
        {item.relations.length ? (
          <>
            <Heading>Relations</Heading>
            {item.relations.map((r) => (
              <RelationLine key={`${r.kind}:${r.fromId}:${r.toId}`} item={item} relation={r} fetch={settled} />
            ))}
          </>
        ) : null}
        <Heading>Description</Heading>
        <Text wrap="wrap" dimColor={!item.description}>
          {item.description ? sanitize(item.description, { multiline: true }) : "—"}
        </Text>
        {item.privateNotes ? (
          <>
            <Heading>Private notes</Heading>
            <Text wrap="wrap">{sanitize(item.privateNotes, { multiline: true })}</Text>
          </>
        ) : null}
      </Box>
    </Box>
  );
}
