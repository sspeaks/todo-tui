import { ClientsProvider, type LiveStore, LiveStoreProvider, type TodoClients } from "@todo-tui/client";
import { Box, Text, useApp, useInput, usePaste, useWindowSize } from "ink";
import { useEffect, useSyncExternalStore } from "react";
import { DetailPane } from "./components/DetailPane.js";
import { HelpOverlay } from "./components/HelpOverlay.js";
import { ControllerContext, useRows, useUi } from "./components/hooks.js";
import { ListPane } from "./components/ListPane.js";
import { MessageLine, PromptPreviewLine } from "./components/MessageLine.js";
import { QuickAddBar } from "./components/QuickAddBar.js";
import { StatusBar } from "./components/StatusBar.js";
import { TabBar } from "./components/TabBar.js";
import type { Controller } from "./controller/controller.js";
import { type Layout, MIN_COLUMNS, MIN_ROWS, layout } from "./state/layout.js";
import { reconcile } from "./state/selection.js";
import type { TabId } from "./state/types.js";

/** The TUI with its providers: a started LiveStore, the RPC clients, and the controller driving it. */
export function Tui({ store, clients, controller }: { store: LiveStore; clients: TodoClients; controller: Controller }) {
  return (
    <LiveStoreProvider store={store}>
      <ClientsProvider clients={clients}>
        <App controller={controller} />
      </ClientsProvider>
    </LiveStoreProvider>
  );
}

/** The shell (design 2.2): tab bar, list (and detail), status bar, message line; exactly the terminal's rows. */
export function App({ controller }: { controller: Controller }) {
  const { exit, suspendTerminal } = useApp();
  const { columns, rows } = useWindowSize();
  useEffect(() => controller.setSize(columns, rows), [controller, columns, rows]);
  useEffect(() => {
    controller.onQuit = () => exit();
    controller.suspendTerminal = suspendTerminal;
    controller.start();
    return () => {
      controller.stop();
      controller.onQuit = undefined;
      controller.suspendTerminal = undefined;
    };
  }, [controller, exit, suspendTerminal]);
  useInput((input, key) => controller.input(input, key));
  // Always on, so that a paste in normal mode never runs as keys.
  usePaste((text) => controller.paste(text));

  // Above the ControllerContext, so straight from the UiStore: the rows quick-add and a prompt take.
  const mode = useSyncExternalStore(controller.ui.subscribe, () => controller.ui.getState().mode.kind);
  const shape = layout(columns, rows, mode);
  return (
    <ControllerContext.Provider value={controller}>
      {shape.tooSmall ? (
        <Text wrap="truncate-end">
          Terminal too small (need {MIN_COLUMNS}×{MIN_ROWS})
        </Text>
      ) : (
        <Box flexDirection="column" width={columns} height={rows}>
          <TabBar width={columns} />
          {mode === "quickAdd" ? <QuickAddBar width={columns} /> : null}
          <Body shape={shape} />
          {mode === "prompt" ? <PromptPreviewLine width={columns} /> : null}
          <StatusBar width={columns} />
          <MessageLine width={columns} />
        </Box>
      )}
    </ControllerContext.Provider>
  );
}

function Body({ shape }: { shape: Layout }) {
  const tab = useUi((s) => s.tab);
  // Remounted per narrowing too, so that rows `/` brings in are not lit up as new.
  const narrow = useUi((s) => s.narrow?.shown);
  const key = `${tab}\0${narrow ?? ""}`;
  const help = useUi((s) => s.mode.kind === "help");
  const overlay = useUi((s) => s.overlay.kind === "detail");
  const focused = useUi((s) => s.focus === "detail");
  const height = shape.bodyHeight;
  if (help) return <HelpOverlay width={shape.columns} height={height} />;
  if (tab === undefined) return <Box height={height} />;
  if (shape.wide) {
    return (
      <Box height={height}>
        <ListPane key={key} tab={tab} width={shape.listWidth} height={height} />
        <Box width={1} height={height} flexShrink={0}>
          <Text color={focused ? "cyan" : "gray"}>{Array.from({ length: height }, () => "│").join("\n")}</Text>
        </Box>
        <SelectedDetail tab={tab} width={shape.detailWidth} height={height} focused={focused} />
      </Box>
    );
  }
  if (overlay) return <SelectedDetail tab={tab} width={shape.columns} height={height} focused />;
  return <ListPane key={key} tab={tab} width={shape.listWidth} height={height} />;
}

/** The detail of the tab's selected Item. */
function SelectedDetail({ tab, width, height, focused }: { tab: TabId; width: number; height: number; focused: boolean }) {
  const { snap, ids, list } = useRows(tab);
  const id = snap.status === "ready" ? reconcile(list, ids, height).selectedId : list.selectedId;
  return <DetailPane id={id} width={width} height={height} focused={focused} />;
}
