import { type Item, errorReason, useConnection, useViews } from "@todo-tui/client";
import { Box, Text } from "ink";
import { type RowCells, fitWaiting, priorityLabel, rowCells, statusGlyph } from "../format/item.js";
import { displayWidth, fit, singleLine } from "../format/text.js";
import { GAP, type RowColumns, rowColumns } from "../state/layout.js";
import { reconcile } from "../state/selection.js";
import { searchQuery } from "../state/tabs.js";
import type { TabId } from "../state/types.js";
import { useFreshIds } from "../useFreshIds.js";
import { useRows, useToday, useUi } from "./hooks.js";

/** What an empty view says, by its seed (design 2.4). */
const EMPTY: Readonly<Record<string, string>> = {
  inbox: "Inbox zero · a to add",
  today: "Nothing due today",
  next: "No next actions",
  waiting: "Not waiting on anyone",
  someday: "Someday is empty",
  archive: "Nothing done or archived yet",
};

const describe = (err: unknown) => (err instanceof Error ? err.message : String(err));

/**
 * A tab's list, windowed: only the rows that fit are rendered (Ink lays out
 * every row it is given). Mount it per tab (key={tab}), so the new-row
 * highlight starts from that view's rows.
 */
export function ListPane({ tab, width, height }: { tab: TabId; width: number; height: number }) {
  const { snap, ids, list } = useRows(tab);
  const live = useConnection().state === "live";
  const focus = useUi((s) => s.focus);
  const seed = useViews().views.find((v) => v.id === tab)?.seed;
  const today = useToday();
  const fresh = useFreshIds(ids, snap.status === "ready");
  const marks = useUi((s) => s.marks);
  const narrow = useUi((s) => s.narrow?.shown);
  const search = searchQuery(tab);

  let body;
  if (snap.status === "loading") {
    body = <Text dimColor>Loading…</Text>;
  } else if (snap.status === "error") {
    const gone = errorReason(snap.error) === "VIEW_NOT_FOUND";
    body = (
      <Text color="red" wrap="truncate-end">
        {gone ? "This view no longer exists" : `Could not load this view: ${singleLine(describe(snap.error))}`}
      </Text>
    );
  } else if (!snap.items.length) {
    let empty = EMPTY[seed ?? ""] ?? "No Items match this view";
    if (search !== undefined) empty = `No Items match “${singleLine(search)}”`;
    if (narrow !== undefined) empty = `Nothing here matches “${singleLine(narrow)}” · esc clears`;
    body = <Text dimColor wrap="truncate-end">{empty}</Text>;
  } else {
    const r = reconcile(list, ids, height);
    const rows = snap.items.slice(r.offset, r.offset + height);
    const cells = rows.map((item) => rowCells(item, today));
    const widest = (pick: (c: RowCells) => string) => Math.max(0, ...cells.map((c) => displayWidth(pick(c))));
    // A cell of margin on the right, before the detail's separator.
    const cols = rowColumns(width - 1, {
      key: widest((c) => c.key),
      due: widest((c) => c.due),
      waiting: widest((c) => c.waiting),
      markers: widest((c) => c.markers),
    });
    body = rows.map((item, i) => (
      <Row
        key={item.id}
        item={item}
        cells={cells[i] ?? rowCells(item, today)}
        cols={cols}
        selected={item.id === r.selectedId}
        marked={marks.has(item.id)}
        active={focus === "list"}
        fresh={fresh.has(item.id)}
        dim={!live}
      />
    ));
  }
  return (
    <Box flexDirection="column" width={width} height={height} overflow="hidden">
      {body}
    </Box>
  );
}

const cell = (text: string, width: number) => (width ? " ".repeat(GAP) + fit(text, width) : "");

interface RowProps {
  item: Item;
  cells: RowCells;
  cols: RowColumns;
  selected: boolean;
  /** Marked (m, V): a • after the selection marker, the row in magenta. */
  marked: boolean;
  /** The list has focus: the selection shows in inverse. */
  active: boolean;
  fresh: boolean;
  /** Not live: the rows may be out of date. */
  dim: boolean;
}

/** One row: "› ○ +3  title  ^Fri  ⧗dana 3d  ⏸Nov 3" ("›•○" when marked), archived rows dimmed. */
function Row({ item, cells, cols, selected, marked, active, fresh, dim }: RowProps) {
  const archived = item.archived !== undefined;
  return (
    <Text
      wrap="truncate-end"
      inverse={selected && active}
      dimColor={dim || archived}
      color={fresh ? "green" : marked ? "magenta" : undefined}
      bold={fresh || marked}
    >
      {selected ? "›" : " "}
      {marked ? "•" : " "}
      {statusGlyph(item.status)} {fit(priorityLabel(item), 3)} {fit(singleLine(item.title), cols.title)}
      {cell(cells.key, cols.key)}
      <Text color={cells.overdue ? "red" : undefined}>{cell(cells.due, cols.due)}</Text>
      <Text color={cells.followUpDue ? "yellow" : undefined} bold={cells.followUpDue}>
        {cols.waiting ? " ".repeat(GAP) + fitWaiting(cells, cols.waiting) : ""}
      </Text>
      {cell(cells.markers, cols.markers)}
    </Text>
  );
}
