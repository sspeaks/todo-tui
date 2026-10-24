// Thin helpers over ItemService's mutating RPCs (design 4.3 rule 4): they fill
// request_id with a fresh ULID and pass expected_revision through. The daemon
// does all validation.

import type { MessageInitShape } from "@bufbuild/protobuf";
import type { CallOptions, Client } from "@connectrpc/connect";
import type { ItemSchema, Status } from "./gen/todo/v1/item_pb.js";
import type {
  ArchiveItemResponse,
  BatchMutateResponse,
  DeleteItemResponse,
  ItemService,
  MutationSchema,
  SetStatusResponse,
  UndeleteItemResponse,
  UnarchiveItemResponse,
  UpdateItemResponse,
} from "./gen/todo/v1/item_service_pb.js";
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
