// Bulk actions (Decision 8): with marks in the tab, the mark-aware keys
// x X s p ^ D w W t z dd act on every marked Item through one BatchMutate,
// in list order and without expected_revision (they are absolute): one op,
// so one undo entry, toasted with its OpSummary. An Item deleted elsewhere
// meanwhile is dropped and the rest retried once.

import {
  type BatchMutateResponse,
  ConnectError,
  ErrorInfoSchema,
  type Item,
  type ItemSchema,
  type MessageInitShape,
  type MutationSchema,
  batchMutate,
  errorReason,
} from "@todo-tui/client";
import type { Target } from "../state/types.js";
import type { Controller } from "./controller.js";
import { errorText, opText } from "./messages.js";

export type MutationInit = MessageInitShape<typeof MutationSchema>;

/** One Item's part of a bulk action. */
export interface BulkStep {
  readonly id: string;
  readonly title: string;
  readonly mutation: MutationInit;
}

/** The most mutations one BatchMutate takes (item_service.proto); splitting would break the one undo entry. */
export const MAX_BULK = 1000;
export const TOO_MANY = `Bulk actions take at most ${MAX_BULK} items`;

/** The Item an ITEM_DELETED or ITEM_NOT_FOUND error names (ErrorInfo metadata item_id). */
export function deletedItemId(err: unknown): string | undefined {
  const reason = errorReason(err);
  if (reason !== "ITEM_DELETED" && reason !== "ITEM_NOT_FOUND") return undefined;
  return err instanceof ConnectError ? err.findDetails(ErrorInfoSchema)[0]?.metadata.item_id : undefined;
}

/**
 * What a batch did, as OpSummary words it (crates/todo-store/src/summary.rs):
 * `mark done (3 items)` for a status, `archive 3 items` for the rest, the
 * title for one. The toast's words when another op landed on top first.
 */
export function bulkText(verb: string, steps: readonly BulkStep[], status = false): string {
  const [first] = steps;
  if (steps.length === 1 && first) return opText(verb, first.title);
  return status ? `${verb} (${steps.length} items)` : `${verb} ${steps.length} items`;
}

/** One step per Item `mutation` returns one for, in the target's order. */
export const stepsOf = (items: readonly Item[], mutation: (item: Item) => MutationInit | undefined): BulkStep[] =>
  items.flatMap((item) => {
    const m = mutation(item);
    return m ? [{ id: item.id, title: item.title, mutation: m }] : [];
  });

/** A field update as one batch mutation. */
export const updateStep = (item: MessageInitShape<typeof ItemSchema> & { id: string }, paths: readonly string[]): MutationInit => ({
  kind: { case: "updateItem", value: { item, updateMask: { paths: [...paths] } } },
});

export class Bulk {
  constructor(private readonly c: Controller) {}

  /** The target's Items as the store holds them, in its (list) order. */
  items(target: Target): Item[] {
    return target.ids.flatMap((id) => {
      const item = this.c.store.getItem(id);
      return item ? [item] : [];
    });
  }

  /**
   * One BatchMutate of `steps`, through the pipeline. When an Item was
   * deleted elsewhere meanwhile (ITEM_DELETED or ITEM_NOT_FOUND naming it),
   * it is dropped and the rest retried once, with a new request_id, and
   * the toast says so. On success the marks clear and the selection goes
   * to where the first marked row was. Returns whether it saved.
   */
  async run(steps: readonly BulkStep[], describe: (done: readonly BulkStep[]) => string): Promise<boolean> {
    const first = this.c.firstMarkedIndex();
    let todo = steps;
    let dropped = 0;
    for (;;) {
      const batch = todo;
      const out = await this.c.pipeline.run<BatchMutateResponse>({
        call: (opts) => batchMutate(this.c.items, batch.map((s) => s.mutation), opts),
        items: (res) => ({ items: res.items, revision: res.result?.revision ?? 0n }),
      });
      if (out.ok) {
        this.c.clearMarks();
        if (first >= 0) this.c.selectIndex(first);
        this.c.toastOp(out.res.result?.opId, describe(batch), dropped ? ` · ${dropped} was deleted elsewhere` : "");
        return true;
      }
      const gone = deletedItemId(out.err);
      const step = batch.find((s) => s.id === gone);
      if (!dropped && step && batch.length > 1) {
        todo = batch.filter((s) => s !== step);
        dropped++;
        continue;
      }
      this.c.failed(errorText(out.err, step?.title));
      return false;
    }
  }
}
