// The "new worktree" composer's own state: which project, what to call it,
// which agent runs, and the daemon's own validation, mirrored so a bad field
// is caught before a round trip rather than after (`composer.tsx` draws
// this; it keeps none of its own).
//
//   ComposerFields ──validate──▶ FieldError[] (empty = may submit)
//                  ──toArgs────▶ CreateWorktreeArgs ──invoke("worktree_create")──▶ daemon
//
// `CreateWorktreeArgs` and `CreatedWorktree` below mirror
// `crates/ainb-desktop/src/create.rs` on `desktop/p3b-desktop-create`
// (`CreateWorktreeArgs`, `CreatedWorktree`). That phase's TypeScript bindings
// (`bindings/Desktop.ts`) are not on this branch yet; once they land, these
// two types are replaced by an import from there, same as `PaletteEntry` is
// in `palette.ts`.

import type { SessionsView_Serialize } from "../../../ainb-app/bindings/AppState";

/** `crates/ainb-hangar-proto/src/spawn.rs::SpawnAgent::tool_arg`, the daemon's
 * own wire strings for the agent CLIs the composer offers. */
export type SpawnAgentId = "claude" | "codex" | "gemini" | "copilot" | "antigravity";

/** The agent segmented control's rows, in the order Orca lists them. */
export const SPAWN_AGENTS: readonly { id: SpawnAgentId; label: string }[] = [
  { id: "claude", label: "Claude" },
  { id: "codex", label: "Codex" },
  { id: "gemini", label: "Gemini" },
  { id: "copilot", label: "Copilot" },
  { id: "antigravity", label: "Antigravity" },
];

/** Mirrors `crates/ainb-desktop/src/create.rs::CreateWorktreeArgs`; see the
 * module comment. */
export interface CreateWorktreeArgs {
  repo_path: string;
  branch: string | null;
  base: string | null;
  agent: SpawnAgentId;
  model: string | null;
  prompt: string | null;
  name: string | null;
}

/** Mirrors `crates/ainb-desktop/src/create.rs::CreatedWorktree`; see the
 * module comment. */
export interface CreatedWorktree {
  session_id: string;
  tmux_session_name: string;
  worktree_path: string;
  branch: string;
}

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
  agent: SpawnAgentId;
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
 * in the frame's own order. */
export function projectChoices(view: SessionsView_Serialize | undefined): ProjectChoice[] {
  return (view?.workspaces ?? []).map((workspace) => ({ name: workspace.name, path: workspace.path }));
}

/**
 * The project the composer opens on: the sidebar's selected session's own
 * workspace, or the first project the frame lists, or `""` when there are
 * none yet (a person can still pick one once a project loads).
 */
export function defaultProjectPath(view: SessionsView_Serialize | undefined): string {
  const workspaces = view?.workspaces ?? [];
  const selectedId = view?.selected_session_id ?? null;
  if (selectedId !== null) {
    const owner = workspaces.find((workspace) => workspace.sessions.some((session) => session.id === selectedId));
    if (owner) return owner.path;
  }
  return workspaces[0]?.path ?? "";
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

/** Longest accepted `name`, `branch`, `base` or `model`, in bytes: mirrors
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

/** `ainb_hangar_proto::spawn::ref_ok`'s conservative subset of
 * `git check-ref-format`: no leading `-` (it would reach `ainb run` and git
 * as a flag), no whitespace, no `..`, no `@{`, none of `~^:?*[\`, no leading
 * or trailing `/` or `.`, no `.lock` suffix. */
function refOk(value: string, max: number): boolean {
  if (!textOk(value, max)) return false;
  const bad =
    value.startsWith("-") ||
    value.startsWith("/") ||
    value.endsWith("/") ||
    value.startsWith(".") ||
    value.endsWith(".") ||
    value.toLowerCase().endsWith(".lock") ||
    value.includes("..") ||
    value.includes("@{") ||
    value.includes("//") ||
    /[\s~^:?*[\\]/.test(value);
  return !bad;
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
  if (fields.model.trim() !== "" && !textOk(fields.model, SPAWN_FIELD_MAX)) {
    errors.push({ field: "model", message: "Model is too long or has control characters." });
  }
  if (fields.base.trim() !== "" && !refOk(fields.base, SPAWN_FIELD_MAX)) {
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
    // No field in this slice maps to the daemon's tmux session name: the
    // Name field only ever feeds the branch preview above. Absent, so the
    // daemon mints its own `<workspace>-<id8>`.
    name: null,
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
