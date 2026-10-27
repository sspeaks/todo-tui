// Thin helpers over ItemService's mutating RPCs (design 4.3 rule 4): they fill
// request_id with a fresh ULID and pass expected_revision (or, for Undo and
// Redo, expected_op_id) through. The daemon does all validation.

import type { MessageInitShape } from "@bufbuild/protobuf";
import { type Timestamp, timestampFromDate } from "@bufbuild/protobuf/wkt";
import { type CallOptions, type Client, ConnectError } from "@connectrpc/connect";
import { ErrorInfoSchema } from "./gen/google/rpc/error_details_pb.js";
import type { CreatedVia, ItemSchema, Status } from "./gen/todo/v1/item_pb.js";
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
  SetStatusResponse,
  UndeleteItemResponse,
  UnarchiveItemResponse,
  UndoResponse,
  UpdateItemResponse,
} from "./gen/todo/v1/item_service_pb.js";
import type { RelationSchema } from "./gen/todo/v1/relation_pb.js";
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
}

function split({ expectedRevision, requestId, ...call }: MutationOptions) {
  const fields = { requestId: requestId ?? ulid(), ...(expectedRevision !== undefined && { expectedRevision }) };
  return [fields, call] as const;
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
  const [fields, call] = split(opts);
  return client.updateItem({ ...fields, item, updateMask: { paths: [...paths] } }, call);
}

export function setStatus(
  client: ItemsClient,
  id: string,
  status: Status,
  opts: MutationOptions = {},
): Promise<SetStatusResponse> {
  const [fields, call] = split(opts);
  return client.setStatus({ ...fields, id, status }, call);
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
  const [fields, call] = split(opts);
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
export function errorReason(err: unknown): string | undefined {
  return err instanceof ConnectError ? err.findDetails(ErrorInfoSchema)[0]?.reason : undefined;
}
