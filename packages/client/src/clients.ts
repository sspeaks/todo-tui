import { type Client, type Transport, createClient } from "@connectrpc/connect";
import { ItemService } from "./gen/todo/v1/item_service_pb.js";
import { AssistantService } from "./gen/todo/v1/assistant_service_pb.js";
import { PeopleService } from "./gen/todo/v1/people_service_pb.js";
import { SuggestionService } from "./gen/todo/v1/suggestion_service_pb.js";
import { SyncService } from "./gen/todo/v1/sync_service_pb.js";
import { SystemService } from "./gen/todo/v1/system_service_pb.js";
import { ViewService } from "./gen/todo/v1/view_service_pb.js";
import { WatchService } from "./gen/todo/v1/watch_service_pb.js";

export interface TodoClients {
  system: Client<typeof SystemService>;
  items: Client<typeof ItemService>;
  views: Client<typeof ViewService>;
  watch: Client<typeof WatchService>;
  sync: Client<typeof SyncService>;
  suggestions: Client<typeof SuggestionService>;
  assistant: Client<typeof AssistantService>;
  people: Client<typeof PeopleService>;
}

export function createClients(transport: Transport): TodoClients {
  return {
    system: createClient(SystemService, transport),
    items: createClient(ItemService, transport),
    views: createClient(ViewService, transport),
    watch: createClient(WatchService, transport),
    sync: createClient(SyncService, transport),
    suggestions: createClient(SuggestionService, transport),
    assistant: createClient(AssistantService, transport),
    people: createClient(PeopleService, transport),
  };
}
