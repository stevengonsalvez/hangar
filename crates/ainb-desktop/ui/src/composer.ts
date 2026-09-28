// The "new worktree" composer's own state: which project, what to call it,
// which agent runs, and the daemon's own validation, mirrored so a bad field
// is caught before a round trip rather than after (`composer.tsx` draws
// this; it keeps none of its own).
//
//   ComposerFields ──validate──▶ FieldError[] (empty = may submit)
//                  ──toArgs────▶ CreateWorktreeArgs ──invoke("worktree_create")──▶ daemon
//
// `CreateWorktreeArgs`, `CreatedWorktree` and `SpawnAgent` are generated
// from the host (`bindings/Desktop.ts`), so the payload and the list of
// agents are written once, in Rust.

import { invoke } from "@tauri-apps/api/core";
import { createEffect, createSignal, type Accessor } from "solid-js";
import type { SessionsView_Serialize } from "../../../ainb-app/bindings/AppState";
import type { CreatedWorktree, CreateWorktreeArgs, RegisteredProject, SpawnAgent } from "../../bindings/Desktop.ts";

export type { CreatedWorktree, CreateWorktreeArgs, RegisteredProject, SpawnAgent };

/** The agent segmented control's labels, in the order Orca lists them. A
 * `Record` over the generated `SpawnAgent`, so an agent added in Rust fails
 * the type check here until it has a label. */
const SPAWN_AGENT_LABELS: Record<SpawnAgent, string> = {
  claude: "Claude",
  codex: "Codex",
  gemini: "Gemini",
  copilot: "Copilot",
  antigravity: "Antigravity",
};

/** The agent segmented control's rows. */
export const SPAWN_AGENTS: readonly { id: SpawnAgent; label: string }[] = (
  Object.keys(SPAWN_AGENT_LABELS) as SpawnAgent[]
).map((id) => ({ id, label: SPAWN_AGENT_LABELS[id] }));

/** The composer's own fields, before anything is derived or sent. Every
 * value is a plain string, even the ones that end up `null` on the wire: a
 * text input has no "absent" of its own, so blank IS how a person clears one
 * (`toArgs` turns blank back into `null`, `crates/ainb-desktop/src/create.rs`'s
 * `given` does the same on the daemon's side of a re-typed value). */
export interface ComposerFields {
  /** The repo path a project's select option carries. */
  projectPath: string;
  /** The smart-name field: sanitized into the branch preview when Advanced's
   * own Branch field is blank. Parsing a `#PR` or a URL out of it is a later
   * phase (reserved, hidden here); a plain name is all this slice reads. */
  name: string;
  agent: SpawnAgent;
  model: string;
  prompt: string;
  /** Advanced: an explicit branch, overriding the preview. */
  branch: string;
  /** Advanced: "Create from" - the base ref, blank meaning the repo default. */
  base: string;
}

/** One project the composer may create into. */
export interface ProjectChoice {
  name: string;
  path: string;
}

/** The Project select's rows: one per workspace the Sessions frame knows,
 * in the frame's own order, then each registered project (`projects_list`,
 * Orca's "a project is listed before it has a worktree") the frame does not
 * already name. */
export function projectChoices(
  view: SessionsView_Serialize | undefined,
  registered: readonly RegisteredProject[] = [],
): ProjectChoice[] {
  const choices = (view?.workspaces ?? []).map((workspace) => ({ name: workspace.name, path: workspace.path }));
  const listed = new Set(choices.map((choice) => choice.path));
  for (const project of registered) {
    if (listed.has(project.path)) continue;
    listed.add(project.path);
    choices.push({ name: project.name, path: project.path });
  }
  return choices;
}

/**
 * The project the composer opens on: the sidebar's selected session's own
 * workspace, or the first project listed, or `""` when there are none yet
 * (a person can still pick one once a project loads).
 */
export function defaultProjectPath(
  view: SessionsView_Serialize | undefined,
  registered: readonly RegisteredProject[] = [],
): string {
  const workspaces = view?.workspaces ?? [];
  const selectedId = view?.selected_session_id ?? null;
  if (selectedId !== null) {
    const owner = workspaces.find((workspace) => workspace.sessions.some((session) => session.id === selectedId));
    if (owner) return owner.path;
  }
  return projectChoices(view, registered)[0]?.path ?? "";
}

/** The registered projects, from the host. A host without the command, or
 * one that fails, answers none: the frame's own projects still show. */
export function loadRegisteredProjects(): Promise<RegisteredProject[]> {
  return invoke<RegisteredProject[] | null>("projects_list").then(
    (projects) => projects ?? [],
    () => [],
  );
}

/** The fresh composer, opened on `view`'s default project. */
export function initialFields(view: SessionsView_Serialize | undefined): ComposerFields {
  return {
    projectPath: defaultProjectPath(view),
    name: "",
    agent: "claude",
    model: "",
    prompt: "",
    branch: "",
    base: "",
  };
}

/** The agent-agnostic branch prefix: Orca's own, not the CLI's name, so a
 * branch reads the same whichever agent runs in it. */
export const BRANCH_PREFIX = "ainb";

/**
 * `name`, as a branch segment: lowercased, every run of characters outside
 * `[a-z0-9._-]` folded to one `-`, and leading or trailing `-` trimmed.
 * Matches Orca's own smart-name sanitizer, so the preview this shows is the
 * branch the daemon will actually make.
 */
export function sanitizeBranchSegment(name: string): string {
  return name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9._-]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

/** The read-only branch preview `name` would make, or `null` when it
 * sanitizes to nothing (a blank name, or one that is only punctuation). */
export function branchPreview(name: string): string | null {
  const segment = sanitizeBranchSegment(name);
  return segment === "" ? null : `${BRANCH_PREFIX}/${segment}`;
}

/**
 * The branch the composer would actually submit: Advanced's own Branch field
 * when it holds one, else the preview built from the Name field, else
 * `null` (the daemon then mints its own `ainb/session-<id8>`).
 */
export function effectiveBranch(fields: ComposerFields): string | null {
  const explicit = fields.branch.trim();
  return explicit !== "" ? explicit : branchPreview(fields.name);
}

/** Longest accepted `branch`, `base` or `model`, in bytes: mirrors
 * `ainb_hangar_proto::spawn::SPAWN_FIELD_MAX`. */
export const SPAWN_FIELD_MAX = 200;

/** Longest accepted first prompt, in bytes: mirrors
 * `ainb_hangar_proto::spawn::SPAWN_PROMPT_MAX`. */
export const SPAWN_PROMPT_MAX = 64 * 1024;

/** `value.len()` in Rust is bytes, not UTF-16 code units: match it so a
 * multi-byte name is not accepted here and refused at the daemon. */
function byteLength(value: string): number {
  return new TextEncoder().encode(value).length;
}

/** A control character C0/C1/DEL: mirrors Rust's `char::is_control`. */
const CONTROL_CHAR = /[\p{Cc}]/u;

/** `ainb_hangar_proto::spawn::text_ok`: not blank, within `max` bytes, and
 * free of control characters. */
function textOk(value: string, max: number): boolean {
  return value.trim() !== "" && byteLength(value) <= max && !CONTROL_CHAR.test(value);
}

/** `ainb_hangar_proto::spawn::ref_ok`: a conservative subset of
 * `git check-ref-format`, over the whole name and per `/` component.
 *
 * Whole name: no leading `-` (it would reach `ainb run` and git as a flag),
 * no whitespace, none of `~^:?*[\`, no `..`, no `@{`, not `@` alone, no
 * trailing `/` or `.`. Each component: not empty, no leading `.`, no `.lock`
 * suffix. The daemon's table of cases (`spawn_validation.json`) pins both
 * sides to the same answers. */
function refOk(value: string, max: number): boolean {
  if (!textOk(value, max)) return false;
  const wholeBad =
    value.startsWith("-") ||
    value === "@" ||
    value.endsWith("/") ||
    value.endsWith(".") ||
    value.includes("..") ||
    value.includes("@{") ||
    /[\s~^:?*[\\]/.test(value);
  const partBad = value
    .split("/")
    .some((part) => part === "" || part.startsWith(".") || part.toLowerCase().endsWith(".lock"));
  return !(wholeBad || partBad);
}

/** The composer's own fields that carry a possible error, so a message can
 * be pinned to the input a person is looking at. `name` covers a bad
 * DERIVED branch (the preview), since that field is the one they typed. */
export type ComposerFieldName = "projectPath" | "model" | "branch" | "base" | "prompt" | "name";

export interface FieldError {
  field: ComposerFieldName;
  message: string;
}

/**
 * Every field that would make the daemon refuse `worktree/create`, mirroring
 * `WorktreeCreateParams::validate` (`ainb_hangar_proto::spawn`) over the
 * fields as `toArgs` would send them: a blank optional is never checked,
 * since it reaches the daemon as `null`, not as itself.
 */
export function validate(fields: ComposerFields): FieldError[] {
  const errors: FieldError[] = [];
  if (fields.projectPath.trim() === "") {
    errors.push({ field: "projectPath", message: "Choose a project." });
  } else if (!fields.projectPath.startsWith("/")) {
    errors.push({ field: "projectPath", message: "The project path must be absolute." });
  }
  // Checked as sent: `toArgs` trims these, so padding a person typed around a
  // value must not refuse what the daemon would accept.
  const model = fields.model.trim();
  if (model !== "" && !textOk(model, SPAWN_FIELD_MAX)) {
    errors.push({ field: "model", message: "Model is too long or has control characters." });
  }
  const base = fields.base.trim();
  if (base !== "" && !refOk(base, SPAWN_FIELD_MAX)) {
    errors.push({ field: "base", message: "Not a valid git ref." });
  }
  const explicitBranch = fields.branch.trim();
  if (explicitBranch !== "") {
    if (!refOk(explicitBranch, SPAWN_FIELD_MAX)) errors.push({ field: "branch", message: "Not a valid git ref." });
  } else {
    const preview = branchPreview(fields.name);
    if (preview !== null && !refOk(preview, SPAWN_FIELD_MAX)) {
      errors.push({ field: "name", message: "This name makes an invalid branch; try Advanced > Branch instead." });
    }
  }
  if (fields.prompt !== "" && (byteLength(fields.prompt) > SPAWN_PROMPT_MAX || fields.prompt.includes("\0"))) {
    errors.push({ field: "prompt", message: "Prompt is too long or has a null byte." });
  }
  return errors;
}

/** Blank (after trimming) becomes `null`: `crates/ainb-desktop/src/create.rs`'s
 * `given`, for the fields it trims. */
function trimmedOrNull(value: string): string | null {
  const trimmed = value.trim();
  return trimmed === "" ? null : trimmed;
}

/** The prompt is kept exactly as typed when it is not blank: `create.rs`
 * only filters on `trim().is_empty()`, it never trims the stored text. */
function promptOrNull(value: string): string | null {
  return value.trim() === "" ? null : value;
}

/** `fields` as the `worktree_create` command's payload. Never checks
 * validity itself; a caller gates on `validate(fields).length === 0` first,
 * the same way the Create button does. */
export function toArgs(fields: ComposerFields): CreateWorktreeArgs {
  return {
    repo_path: fields.projectPath.trim(),
    branch: trimmedOrNull(effectiveBranch(fields) ?? ""),
    base: trimmedOrNull(fields.base),
    agent: fields.agent,
    model: trimmedOrNull(fields.model),
    prompt: promptOrNull(fields.prompt),
  };
}

/** The create request's own progress, held above the composer's view so a
 * Cancel that only closes the overlay (`composer.tsx`) does not lose it:
 * the request already in flight keeps running on the host either way. */
export type CreateState =
  | { kind: "idle" }
  | { kind: "creating" }
  | { kind: "failed"; message: string }
  | { kind: "done"; result: CreatedWorktree };

/** What the sidebar's pending card (`sidebar.tsx`) needs while a create is in
 * flight: enough to place it in the right project's group and label it. Held
 * beside `CreateState` rather than inside it, because the card outlives the
 * composer's own view (it is drawn from `main.tsx`, not from `composer.tsx`). */
export interface PendingWorktree {
  projectPath: string;
  name: string;
}

/** What the create flow needs from the window it runs in. */
export interface ComposerFlowDeps {
  /** The host call. Absent: the real `worktree_create` command. */
  create?(args: CreateWorktreeArgs): Promise<CreatedWorktree>;
  /** A failure that lands after the view closed, so it is not lost. */
  toast(message: string): void;
  /** Where the keyboard goes when the view closes. */
  restoreFocus(): void;
  /** The session list as the window last drew it. */
  sessions(): SessionsView_Serialize | undefined;
  /** Select `sessionId`'s row in the session list, without attaching it: the
   * host already opened its tab. */
  select(sessionId: string): void;
  /** The clock the follow deadline reads. Absent: `Date.now`. */
  now?(): number;
}

/** How long a created session may take to be listed before the flow stops
 * waiting to select it: past this a late row would yank the selection from
 * whatever the person has moved on to. */
export const FOLLOW_MS = 10_000;

/** A created session waiting for its row: which, since when, and what the
 * session list had selected when the create finished. */
interface Following {
  id: string;
  since: number;
  selectedAtCreate: string | null;
}

/** The composer's open state, its request's progress, and the sidebar's
 * pending card, with the actions that move them. */
export interface ComposerFlow {
  open: Accessor<boolean>;
  state: Accessor<CreateState>;
  pending: Accessor<PendingWorktree | null>;
  openComposer(): void;
  closeComposer(): void;
  submit(fields: ComposerFields): void;
}

/**
 * The create flow the window runs, in one place so the window and its test
 * run the same code. The request's progress lives here, not in the view:
 * Cancel (or Esc) closing the view never stops a create already on the
 * host, the pending card draws until the host answers, a success closes the
 * view, and a failure the view is no longer open to show becomes a toast.
 *
 * A success also selects the new session, as Orca activates and reveals a
 * worktree it just created: the host opens the new tab, and this moves the
 * session list's selection (the sidebar row and the answer banner's scope)
 * onto it too. The daemon lists the session on its own schedule, so the
 * selection waits until the list carries the row, for at most `FOLLOW_MS`,
 * and gives up the moment the list's selection moves off the row it had
 * when the create finished: a pick from the sidebar, a tab, the board or the
 * palette all move that one selection, so any of them wins over a late row.
 */
export function createComposerFlow(deps: ComposerFlowDeps): ComposerFlow {
  const create = deps.create ?? ((args: CreateWorktreeArgs) => invoke<CreatedWorktree>("worktree_create", { args }));
  const [open, setOpen] = createSignal(false);
  const [state, setState] = createSignal<CreateState>({ kind: "idle" });
  const [pending, setPending] = createSignal<PendingWorktree | null>(null);
  const now = deps.now ?? Date.now;
  // The created session waiting for its row, selected once, when it lands.
  const [following, setFollowing] = createSignal<Following | null>(null);
  createEffect(() => {
    const wait = following();
    if (wait === null) return;
    const view = deps.sessions();
    const selected = view?.selected_session_id ?? null;
    if (selected === wait.id) return setFollowing(null);
    if (selected !== wait.selectedAtCreate || now() - wait.since > FOLLOW_MS) return setFollowing(null);
    if (!listed(view, wait.id)) return;
    setFollowing(null);
    deps.select(wait.id);
  });

  const closeComposer = () => {
    if (!open()) return;
    setOpen(false);
    deps.restoreFocus();
  };
  return {
    open,
    state,
    pending,
    openComposer() {
      // Reopening over a finished or failed request starts fresh; reopening
      // over one still running shows it running.
      if (state().kind !== "creating") setState({ kind: "idle" });
      setOpen(true);
    },
    closeComposer,
    submit(fields) {
      // The view disables Create while invalid; this is the same check, so a
      // stray Enter can never send what the daemon would refuse.
      if (state().kind === "creating" || validate(fields).length > 0) return;
      setPending({ projectPath: fields.projectPath, name: fields.name.trim() || "New worktree" });
      setState({ kind: "creating" });
      create(toArgs(fields)).then(
        (result) => {
          setPending(null);
          setState({ kind: "done", result });
          setFollowing({
            id: result.session_id,
            since: now(),
            selectedAtCreate: deps.sessions()?.selected_session_id ?? null,
          });
          closeComposer();
        },
        (error: unknown) => {
          setPending(null);
          const message = String(error);
          setState({ kind: "failed", message });
          if (!open()) deps.toast(message);
        },
      );
    },
  };
}

/** Whether the session list carries a row for `sessionId`. */
function listed(view: SessionsView_Serialize | undefined, sessionId: string): boolean {
  return view?.workspaces.some((workspace) => workspace.sessions.some((row) => row.id === sessionId)) ?? false;
}
