// A StoreCache as plain JSON, to keep between launches: each message as
// protobuf binary in base64 (as a view's key already is), so a field the
// app does not know yet survives, and anything malformed reads as no cache.
import { type DescMessage, type MessageShape, fromBinary, toBinary } from "@bufbuild/protobuf";
import { base64Decode, base64Encode } from "@bufbuild/protobuf/wire";
import { ItemSchema } from "./gen/todo/v1/item_pb.js";
import { StatusCountsSchema } from "./gen/todo/v1/item_service_pb.js";
import { ViewSchema } from "./gen/todo/v1/view_pb.js";
import type { StoreCache } from "./live-store.js";

/** Bumped when the shape changes: an older cache then reads as none. */
const VERSION = 1;

export interface EncodedStoreCache {
  v: number;
  views: string[];
  lists: { key: string; ids: string[] }[];
  items: string[];
  counts?: { counts?: string; views: [string, number][] };
}

const encode = <D extends DescMessage>(schema: D, m: MessageShape<D>): string => base64Encode(toBinary(schema, m));
const decode = <D extends DescMessage>(schema: D, s: unknown): MessageShape<D> => {
  if (typeof s !== "string") throw new TypeError("not a string");
  return fromBinary(schema, base64Decode(s));
};

export function encodeStoreCache(c: StoreCache): EncodedStoreCache {
  const counts = c.counts;
  return {
    v: VERSION,
    views: c.views.map((v) => encode(ViewSchema, v)),
    lists: c.lists.map((l) => ({ key: l.key, ids: [...l.ids] })),
    items: c.items.map((i) => encode(ItemSchema, i)),
    ...(counts && {
      counts: { ...(counts.counts !== undefined && { counts: encode(StatusCountsSchema, counts.counts) }), views: [...counts.views] },
    }),
  };
}

const isStrings = (x: unknown): x is string[] => Array.isArray(x) && x.every((s) => typeof s === "string");

/** The cache `x` holds, or undefined when it is not one this version wrote (or is malformed). */
export function decodeStoreCache(x: unknown): StoreCache | undefined {
  try {
    const e = x as Partial<EncodedStoreCache> | null;
    if (!e || e.v !== VERSION || !Array.isArray(e.views) || !Array.isArray(e.lists) || !Array.isArray(e.items)) return undefined;
    const lists = e.lists.map((l: unknown) => {
      const { key, ids } = (l ?? {}) as { key?: unknown; ids?: unknown };
      if (typeof key !== "string" || !isStrings(ids)) throw new TypeError("bad list");
      return { key, ids };
    });
    let counts: StoreCache["counts"];
    if (e.counts !== undefined) {
      const views = e.counts.views;
      if (!Array.isArray(views) || !views.every((p) => Array.isArray(p) && typeof p[0] === "string" && typeof p[1] === "number")) return undefined;
      counts = {
        ...(e.counts.counts !== undefined && { counts: decode(StatusCountsSchema, e.counts.counts) }),
        views: new Map(views),
      };
    }
    return {
      views: e.views.map((v) => decode(ViewSchema, v)),
      lists,
      items: e.items.map((i) => decode(ItemSchema, i)),
      ...(counts && { counts }),
    };
  } catch {
    return undefined;
  }
}
