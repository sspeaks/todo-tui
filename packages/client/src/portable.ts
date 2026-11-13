// The subset of @todo-tui/client with no Node APIs and no React: what the
// phone app bundles. The Unix-socket transport, spawning and paths stay in
// core; the FakeDaemon is "./fake".
export * from "./gen/todo/v1/common_pb.js";
export * from "./gen/todo/v1/config_pb.js";
export * from "./gen/todo/v1/field_pb.js";
export * from "./gen/todo/v1/field_value_pb.js";
export * from "./gen/todo/v1/filter_pb.js";
export * from "./gen/todo/v1/item_pb.js";
export * from "./gen/todo/v1/item_service_pb.js";
export * from "./gen/todo/v1/op_pb.js";
export * from "./gen/todo/v1/relation_pb.js";
export * from "./gen/todo/v1/suggestion_pb.js";
export * from "./gen/todo/v1/assistant_service_pb.js";
export * from "./gen/todo/v1/suggestion_service_pb.js";
export * from "./gen/todo/v1/sync_service_pb.js";
export * from "./gen/todo/v1/system_service_pb.js";
export * from "./gen/todo/v1/view_pb.js";
export * from "./gen/todo/v1/view_service_pb.js";
export * from "./gen/todo/v1/watch_service_pb.js";
export {
  type BadRequest,
  BadRequestSchema,
  type BadRequest_FieldViolation,
  type ErrorInfo,
  ErrorInfoSchema,
  type PreconditionFailure,
  PreconditionFailureSchema,
} from "./gen/google/rpc/error_details_pb.js";
export { type Date as ProtoDate, DateSchema as ProtoDateSchema } from "./gen/google/type/date_pb.js";

export { type MessageInitShape, clone, create, equals, fromJson, toJson, toJsonString } from "@bufbuild/protobuf";
export {
  type FieldMask,
  FieldMaskSchema,
  type Timestamp,
  timestampDate,
  timestampFromDate,
  timestampNow,
} from "@bufbuild/protobuf/wkt";
export {
  type CallOptions,
  type Client,
  Code,
  ConnectError,
  type ConnectRouter,
  type Transport,
  createClient,
  createRouterTransport,
} from "@connectrpc/connect";

export * from "./clients.js";
export * from "./errors.js";
export * from "./live-store.js";
export * from "./store-cache.js";
export * from "./ulid.js";
export * from "./mutations.js";
export * from "./queries.js";
export * from "./limits.js";
export { ordinalSuffix, ruleFromProto, ruleToken } from "./recurrence.js";
export * from "./remote.js";
