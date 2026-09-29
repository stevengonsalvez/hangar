import assert from "node:assert/strict";
import { test } from "node:test";
import type { Session_Serialize } from "../../../ainb-app/bindings/AppState";
import {
  editorIntents,
  opensRowMenu,
  rowMenuItems,
  runRowPick,
  stepItem,
  type RowMenuAction,
  type RowMenuItem,
} from "./row_menu.ts";
import type { RendererIntent } from "./tabs.ts";

function session(over: Partial<Session_Serialize> = {}): Session_Serialize {
  return { id: "s-1", workspace_path: "/repo/wt-a", ssh_target: null, ...over } as Session_Serialize;
}

const enabled = (items: RowMenuItem[]) => items.filter((item) => !item.disabled).map((item) => item.action);

test("a local row with a worktree path offers every wired item", () => {
  const items = rowMenuItems(session());
  assert.deepEqual(
    items.map((item) => item.action),
    ["open", "editor", "copy_path", "copy_name"],
  );
  assert.deepEqual(enabled(items), ["open", "editor", "copy_path", "copy_name"]);
});

test("a row with no worktree path cannot open an editor or copy a path, and says why", () => {
  const items = rowMenuItems(session({ workspace_path: "" }));
  assert.deepEqual(enabled(items), ["open", "copy_name"]);
  for (const item of items.filter((candidate) => candidate.disabled)) {
    assert.ok(item.reason && item.reason.length > 0, `${item.action} names why it is off`);
  }
});

test("a remote row cannot open its path in a local editor, as Orca marks Open in local only", () => {
  const items = rowMenuItems(session({ ssh_target: { host: "box" } as Session_Serialize["ssh_target"] }));
  assert.deepEqual(enabled(items), ["open", "copy_path", "copy_name"]);
  assert.match(items.find((item) => item.action === "editor")?.reason ?? "", /local/i);
});

test("the arrow keys walk the enabled items and wrap, skipping disabled ones", () => {
  const items = rowMenuItems(session({ workspace_path: "" }));
  // open(0) editor(1, off) copy_path(2, off) copy_name(3)
  assert.equal(stepItem(items, 0, 1), 3);
  assert.equal(stepItem(items, 3, 1), 0);
  assert.equal(stepItem(items, 0, -1), 3);
  assert.equal(stepItem(items, -1, 1), 0, "from nothing, down lands on the first");
  assert.equal(stepItem(items, -1, -1), 3, "from nothing, up lands on the last");
});

test("a menu with nothing enabled has nowhere to step", () => {
  const items: RowMenuItem[] = [{ action: "open", label: "Open", disabled: true, reason: "no" }];
  assert.equal(stepItem(items, -1, 1), -1);
});

test("the context-menu key and Shift+F10 open the menu, and nothing else does", () => {
  assert.equal(opensRowMenu({ key: "ContextMenu", shiftKey: false, ctrlKey: false, altKey: false, metaKey: false }), true);
  assert.equal(opensRowMenu({ key: "F10", shiftKey: true, ctrlKey: false, altKey: false, metaKey: false }), true);
  assert.equal(opensRowMenu({ key: "F10", shiftKey: false, ctrlKey: false, altKey: false, metaKey: false }), false);
  assert.equal(opensRowMenu({ key: "F10", shiftKey: true, ctrlKey: true, altKey: false, metaKey: false }), false);
  assert.equal(opensRowMenu({ key: "Enter", shiftKey: false, ctrlKey: false, altKey: false, metaKey: false }), false);
});

test("open in editor selects the row first, then runs the session list's own editor command", () => {
  assert.deepEqual(editorIntents("s-1"), [
    { Command: ["session_list.select_row", { target: { session: "s-1" }, open: false }] },
    { Command: ["session_list.editor", null] },
  ]);
});

test("each pick runs the window's existing action for it, and only that one", () => {
  const ran = (action: RowMenuAction) => {
    const calls: unknown[] = [];
    runRowPick(
      { action, session: session(), name: "wt-a" },
      {
        open: (id: string) => calls.push(["open", id]),
        run: (intents: RendererIntent[]) => calls.push(["run", intents]),
        copy: (text: string) => calls.push(["copy", text]),
      },
    );
    return calls;
  };
  assert.deepEqual(ran("open"), [["open", "s-1"]]);
  assert.deepEqual(ran("editor"), [["run", editorIntents("s-1")]]);
  assert.deepEqual(ran("copy_path"), [["copy", "/repo/wt-a"]]);
  assert.deepEqual(ran("copy_name"), [["copy", "wt-a"]]);
});
