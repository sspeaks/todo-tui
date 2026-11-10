// Thin helpers over ItemService's mutating RPCs (design 4.3 rule 4): they fill
// request_id with a fresh ULID and pass expected_revision (or, for Undo and
// Redo, expected_op_id) through. The daemon does all validation.

import type { MessageInitShape } from "@bufbuild/protobuf";
import { type Timestamp, timestampFromDate } from "@bufbuild/protobuf/wkt";
import { type CallOptions, type Client, ConnectError } from "@connectrpc/connect";
import { ErrorInfoSchema } from "./gen/google/rpc/error_details_pb.js";
import type { CreatedVia, ItemSchema, RecurrenceRuleSchema, Status } from "./gen/todo/v1/item_pb.js";
import type {
  AddRelationResponse,
  ArchiveItemResponse,
  BatchMutateResponse,
  CaptureItemResponse,
  DeleteItemResponse,
  GetUndoStateResponse,
  ItemService,
  MutationSchema,
  RedoResponse,
  RemoveRelationResponse,
  SetRecurrenceResponse,
  SetStatusResponse,
  UndeleteItemResponse,
  UnarchiveItemResponse,
  UndoResponse,
  UpdateItemResponse,
} from "./gen/todo/v1/item_service_pb.js";
import type { FilterSchema, SortKeySchema } from "./gen/todo/v1/filter_pb.js";
import type { RelationSchema } from "./gen/todo/v1/relation_pb.js";
import type {
  ApplySuggestionPatchesResponse,
  RejectSuggestionResponse,
  SuggestionService,
} from "./gen/todo/v1/suggestion_service_pb.js";
import type { ViewSchema } from "./gen/todo/v1/view_pb.js";
import type {
  CreateViewResponse,
  DeleteViewResponse,
  MoveViewResponse,
  UpdateViewResponse,
} from "./gen/todo/v1/view_service_pb.js";
import type { ViewsClient } from "./queries.js";
import { ulid } from "./ulid.js";

export type ItemsClient = Client<typeof ItemService>;

export interface MutationOptions extends CallOptions {
  /**
   * The Item revision the change was made from; the daemon fails with
   * ABORTED (ErrorInfo REVISION_MISMATCH) if the Item changed since.
   */
  expectedRevision?: bigint;
  /** Defaults to a new ULID. Reuse one to retry a call idempotently. */
  requestId?: string;
  /**
   * The person confirmed changing, in the tracker, an issue assigned to
   * someone else (design 5.9): without it such an edit fails with
   * FAILED_PRECONDITION (ErrorInfo NOT_MINE). Only the calls that push take it.
   */
  confirmNotMine?: boolean;
}

function split({ expectedRevision, requestId, confirmNotMine: _, ...call }: MutationOptions) {
  const fields = { requestId: requestId ?? ulid(), ...(expectedRevision !== undefined && { expectedRevision }) };
  return [fields, call] as const;
}

/** As split, keeping confirm_not_mine for the calls whose edits push. */
function splitPushed(opts: MutationOptions) {
  const [fields, call] = split(opts);
  return [{ ...fields, ...(opts.confirmNotMine && { confirmNotMine: true }) }, call] as const;
}

export interface CaptureOptions extends CallOptions {
  /** Who captured it (Item.created_via), e.g. CreatedVia.TUI. */
  createdVia: CreatedVia;
  /** Captures it as a child of this Item, which must be live (else INVALID_ARGUMENT on parent_id). */
  parentId?: string;
  /** "Now" for relative dates (^fri) and created_at; defaults to the daemon's clock. */
  referenceTime?: Date | Timestamp;
  /** Defaults to a new ULID. Reuse one to retry a call idempotently. */
  requestId?: string;
}

/**
 * CaptureItem (design 2.3): the daemon parses `text` (ParseCapture) and
 * creates the Item; the response's draft carries the tokens and warnings.
 */
export function captureItem(client: ItemsClient, text: string, opts: CaptureOptions): Promise<CaptureItemResponse> {
  const { createdVia, parentId, referenceTime, requestId, ...call } = opts;
  const at = referenceTime instanceof Date ? timestampFromDate(referenceTime) : referenceTime;
  return client.captureItem(
    {
      requestId: requestId ?? ulid(),
      text,
      createdVia,
      ...(parentId !== undefined && { parentId }),
      ...(at !== undefined && { referenceTime: at }),
    },
    call,
  );
}

/**
 * UpdateItem with `update_mask` built from `paths` (Item proto field names,
 * e.g. "title" or "waiting.follow_up_on"); `item.id` names the Item.
 */
export function updateItem(
  client: ItemsClient,
  item: MessageInitShape<typeof ItemSchema> & { id: string },
  paths: readonly string[],
  opts: MutationOptions = {},
): Promise<UpdateItemResponse> {
  const [fields, call] = splitPushed(opts);
  return client.updateItem({ ...fields, item, updateMask: { paths: [...paths] } }, call);
}

export function setStatus(
  client: ItemsClient,
  id: string,
  status: Status,
  opts: MutationOptions = {},
): Promise<SetStatusResponse> {
  const [fields, call] = splitPushed(opts);
  return client.setStatus({ ...fields, id, status }, call);
}

/**
 * SetRecurrence (design 3.8): sets or replaces the Item's repeat rule, or
 * clears it when `rule` is undefined. Setting a rule on a done or cancelled
 * Item fails with FAILED_PRECONDITION (ErrorInfo ITEM_CLOSED).
 */
export function setRecurrence(
  client: ItemsClient,
  id: string,
  rule: MessageInitShape<typeof RecurrenceRuleSchema> | undefined,
  opts: MutationOptions = {},
): Promise<SetRecurrenceResponse> {
  const [fields, call] = splitPushed(opts);
  return client.setRecurrence({ ...fields, id, ...(rule !== undefined && { rule }) }, call);
}

export function archiveItem(client: ItemsClient, id: string, opts: MutationOptions = {}): Promise<ArchiveItemResponse> {
  const [fields, call] = split(opts);
  return client.archiveItem({ ...fields, id }, call);
}

export function unarchiveItem(client: ItemsClient, id: string, opts: MutationOptions = {}): Promise<UnarchiveItemResponse> {
  const [fields, call] = split(opts);
  return client.unarchiveItem({ ...fields, id }, call);
}

/** Soft delete (design 3.7). */
export function deleteItem(client: ItemsClient, id: string, opts: MutationOptions = {}): Promise<DeleteItemResponse> {
  const [fields, call] = split(opts);
  return client.deleteItem({ ...fields, id }, call);
}

export function undeleteItem(client: ItemsClient, id: string, opts: MutationOptions = {}): Promise<UndeleteItemResponse> {
  const [fields, call] = split(opts);
  return client.undeleteItem({ ...fields, id }, call);
}

/**
 * BatchMutate: applies `mutations` atomically as one op. Each carries its own
 * expected_revision; their request_id stays empty.
 */
export function batchMutate(
  client: ItemsClient,
  mutations: readonly MessageInitShape<typeof MutationSchema>[],
  opts: Omit<MutationOptions, "expectedRevision"> = {},
): Promise<BatchMutateResponse> {
  const [fields, call] = splitPushed(opts);
  return client.batchMutate({ ...fields, mutations: [...mutations] }, call);
}

export interface UndoOptions extends CallOptions {
  /**
   * The op_id of the entry meant (GetUndoState's `undo`, or `redo` for
   * Redo); the daemon fails with ABORTED (ErrorInfo UNDO_STACK_CHANGED) if
   * another entry, or none, is on top.
   */
  expectedOpId?: string;
  /** Defaults to a new ULID. Reuse one to retry a call idempotently. */
  requestId?: string;
}

function splitUndo({ expectedOpId, requestId, ...call }: UndoOptions) {
  const fields = { requestId: requestId ?? ulid(), ...(expectedOpId !== undefined && { expectedOpId }) };
  return [fields, call] as const;
}

/**
 * Undo (design 3.10): reverts the top entry of the global undo stack. With an
 * empty stack it succeeds with `entry` unset; fields that changed since the
 * entry are left alone and listed in `skipped`.
 */
export function undo(client: ItemsClient, opts: UndoOptions = {}): Promise<UndoResponse> {
  const [fields, call] = splitUndo(opts);
  return client.undo(fields, call);
}

/** Redo: re-applies the most recently undone entry, as Undo reverts one. */
export function redo(client: ItemsClient, opts: UndoOptions = {}): Promise<RedoResponse> {
  const [fields, call] = splitUndo(opts);
  return client.redo(fields, call);
}

/** The entries Undo and Redo would act on next, e.g. for "Undo: <description>". */
export function getUndoState(client: ItemsClient, opts: CallOptions = {}): Promise<GetUndoStateResponse> {
  return client.getUndoState({}, opts);
}

/**
 * AddRelation (design 3.6). A RELATES_TO may name its ends in either order;
 * the response's from_item / to_item are the ends as stored. Adding a
 * relation that exists changes nothing.
 */
export function addRelation(
  client: ItemsClient,
  relation: MessageInitShape<typeof RelationSchema>,
  opts: Omit<MutationOptions, "expectedRevision"> = {},
): Promise<AddRelationResponse> {
  const [fields, call] = split(opts);
  return client.addRelation({ ...fields, relation }, call);
}

/** RemoveRelation; removing a relation that does not exist changes nothing. */
export function removeRelation(
  client: ItemsClient,
  relation: MessageInitShape<typeof RelationSchema>,
  opts: Omit<MutationOptions, "expectedRevision"> = {},
): Promise<RemoveRelationResponse> {
  const [fields, call] = split(opts);
  return client.removeRelation({ ...fields, relation }, call);
}

/**
 * The ErrorInfo reason of a ConnectError, e.g. "REVISION_MISMATCH" or
 * "UNDO_STACK_CHANGED"; undefined for other errors and for errors without
 * an ErrorInfo detail.
 */
// --- views (design 2.4): ops for idempotency, never undo stack entries ---

export interface NewView {
  name: string;
  filter: MessageInitShape<typeof FilterSchema>;
  sort?: MessageInitShape<typeof SortKeySchema>[];
  /** Default true. */
  showCount?: boolean;
  /** 0-based; default last. */
  position?: number;
}

/** CreateView: ALREADY_EXISTS (VIEW_NAME_TAKEN) when another view has the name, ignoring case. */
export function createView(client: ViewsClient, view: NewView, opts: MutationOptions = {}): Promise<CreateViewResponse> {
  const [fields, call] = split(opts);
  return client.createView({ ...fields, ...view, sort: view.sort ?? [], showCount: view.showCount ?? true }, call);
}

/**
 * UpdateView: sets the fields `paths` names (name, filter, sort,
 * show_count) from `view`, which carries the id. `expectedRevision` is the
 * view's revision as rendered (ABORTED / REVISION_MISMATCH if it changed).
 */
export function updateView(
  client: ViewsClient,
  view: MessageInitShape<typeof ViewSchema> & { id: string },
  paths: readonly ("name" | "filter" | "sort" | "show_count")[],
  opts: MutationOptions = {},
): Promise<UpdateViewResponse> {
  const [fields, call] = split(opts);
  return client.updateView({ ...fields, view, updateMask: { paths: [...paths] } }, call);
}

export function deleteView(client: ViewsClient, id: string, opts: MutationOptions = {}): Promise<DeleteViewResponse> {
  const [{ requestId }, call] = split(opts);
  return client.deleteView({ requestId, id }, call);
}

/** MoveView: to `position` (0-based, past the end is last). */
export function moveView(client: ViewsClient, id: string, position: number, opts: MutationOptions = {}): Promise<MoveViewResponse> {
  const [{ requestId }, call] = split(opts);
  return client.moveView({ requestId, id, position }, call);
}

// --- Suggestions (design 3.11) ---

export type SuggestionsClient = Client<typeof SuggestionService>;

/**
 * ApplySuggestionPatches: the patches, in one undoable op; those whose
 * field changed since are skipped (skipped_patch_ids). On a linked Item
 * each value is an edit that pushes, refused as a direct edit is
 * (LINKED_READ_ONLY, NOT_MINE without confirmNotMine).
 */
export function applySuggestionPatches(
  client: SuggestionsClient,
  patchIds: readonly string[],
  opts: Omit<MutationOptions, "expectedRevision"> = {},
): Promise<ApplySuggestionPatchesResponse> {
  const [fields, call] = splitPushed(opts);
  return client.applySuggestionPatches({ ...fields, patchIds: [...patchIds] }, call);
}

/** RejectSuggestion: what waits of it is let go; never an undo stack entry. */
export function rejectSuggestion(
  client: SuggestionsClient,
  id: string,
  opts: Omit<MutationOptions, "expectedRevision" | "confirmNotMine"> = {},
): Promise<RejectSuggestionResponse> {
  const [{ requestId }, call] = split(opts);
  return client.rejectSuggestion({ requestId, id }, call);
}

export function errorReason(err: unknown): string | undefined {
  return err instanceof ConnectError ? err.findDetails(ErrorInfoSchema)[0]?.reason : undefined;
}
