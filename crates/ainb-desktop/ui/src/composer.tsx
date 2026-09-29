import { createEffect, createMemo, createResource, createSignal, For, onCleanup, Show, untrack } from "solid-js";
import type { SessionsView_Serialize } from "../../../ainb-app/bindings/AppState";
import {
  addProject,
  branchPreview,
  initialFields,
  loadRegisteredProjects,
  projectChoices,
  REGISTER_FOLDER_COMMAND,
  SPAWN_AGENTS,
  validate,
  type ComposerFields,
  type ComposerFieldName,
  type CreateState,
  type ProjectChoice,
} from "./composer.ts";

interface Props {
  /** The sessions frame the Project select and the default project come from. */
  sessions: SessionsView_Serialize | undefined;
  /** The in-flight request's own progress, held above this component so a
   * Cancel that only closes the overlay does not lose it (`composer.ts`). */
  state: CreateState;
  /** Submit `fields`, picked from `projects`; the caller checks `validate`
   * again before it sends. */
  onSubmit(fields: ComposerFields, projects: readonly ProjectChoice[]): void;
  /** Esc, a click outside, or Cancel: close the overlay. The request behind
   * `state`, if any, is not this component's to stop. */
  onClose(): void;
}

/** How long a create runs before the button says so: the same
 * feedback-by-duration rule the sidecar and updater already draw by
 * (nothing under this reads as a stall, everything over it says what is
 * happening rather than leaving the button looking merely slow). */
const BUSY_LABEL_MS = 200;

/**
 * The Orca "new worktree" composer: Mod+N's own modal, over the fields this
 * slice sends (`composer.ts`'s `ComposerFields`). A floating surface like the
 * palette's, with an aria-invalid message under whichever field the daemon's
 * own validation would refuse, so a bad ref is caught before the round trip.
 *
 * The fields are seeded once, from the frame this had when it opened
 * (mounted only while open, like `Palette`): a frame that lands mid-edit
 * must not overwrite what a person is typing.
 */
export function Composer(props: Props) {
  const [fields, setFields] = createSignal<ComposerFields>(initialFields(props.sessions));
  const set = <K extends keyof ComposerFields>(key: K, value: ComposerFields[K]) =>
    setFields((current) => ({ ...current, [key]: value }));

  const creating = () => props.state.kind === "creating";
  const failure = () => (props.state.kind === "failed" ? props.state.message : null);
  const canSubmit = () => !creating() && errors().length === 0;

  // The spinner and its label wait out `BUSY_LABEL_MS` before showing, so a
  // create the daemon answers at once never flickers a busy state on and off.
  const [busy, setBusy] = createSignal(false);
  createEffect(() => {
    if (!creating()) {
      setBusy(false);
      return;
    }
    const timer = setTimeout(() => setBusy(true), BUSY_LABEL_MS);
    onCleanup(() => clearTimeout(timer));
  });

  // Read per open (this is mounted only while open), so a folder registered
  // since the last open is offered. It lands after the first paint: a blank
  // project is filled then, and a listed pick is left alone. A pick the new
  // rows no longer carry is cleared, never moved to another row: Create is
  // then refused ("Choose a project.") rather than sent to a repository the
  // person never chose.
  const [registered, { mutate: setRegistered, refetch: refetchRegistered }] = createResource(loadRegisteredProjects, {
    initialValue: [],
  });
  const projects = createMemo(() => projectChoices(props.sessions, registered()));
  const errors = createMemo(() => validate(fields(), projects()));
  const errorFor = (field: ComposerFieldName) => errors().find((error) => error.field === field)?.message ?? null;
  const invalid = (field: ComposerFieldName) => errorFor(field) !== null;
  // Nothing to pick, once the host has answered.
  const empty = () => projects().length === 0 && !registered.loading;
  createEffect(() => {
    const rows = projects();
    const pick = untrack(fields).projectPath;
    if (rows.some((project) => project.path === pick)) return;
    const next = pick === "" ? (rows[0]?.path ?? "") : "";
    if (next !== pick) set("projectPath", next);
  });
  const preview = createMemo(() => branchPreview(fields().name));

  // Add project: the host opens the OS folder picker and registers the pick,
  // so a create from it is accepted. The new project is listed and chosen;
  // a cancelled picker changes nothing; a refusal says why under the field.
  const [adding, setAdding] = createSignal(false);
  const [addError, setAddError] = createSignal<string | null>(null);
  const onAddProject = () => {
    if (adding()) return;
    setAdding(true);
    setAddError(null);
    addProject().then(
      (project) => {
        setAdding(false);
        if (project === null) return;
        // Listed and chosen at once, then the host's own list, which is the
        // truth about what the daemon now accepts.
        setRegistered((current) => [...(current ?? []).filter((known) => known.path !== project.path), project]);
        set("projectPath", project.path);
        void refetchRegistered();
      },
      (error: unknown) => {
        setAdding(false);
        setAddError(String(error));
      },
    );
  };

  const submit = (event: Event) => {
    event.preventDefault();
    if (canSubmit()) props.onSubmit(fields(), projects());
  };

  // Mod+Enter submits from the prompt textarea, where a plain Enter is a
  // newline. Every single-line field submits on a plain Enter for free: the
  // platform's own form submission, not reimplemented here.
  const onPromptKeyDown = (event: KeyboardEvent) => {
    // Not while an input method is composing: its Enter confirms a candidate.
    if ((event.metaKey || event.ctrlKey) && event.key === "Enter" && !event.isComposing) submit(event);
  };

  // A click closes only when it both started and ended on the scrim: a drag
  // that selects text in a field and is released outside it must not throw
  // the form away.
  let downOnScrim = false;
  const onScrimDown = (event: MouseEvent) => {
    downOnScrim = event.target === event.currentTarget;
  };
  const onScrimClick = (event: MouseEvent) => {
    if (downOnScrim && event.target === event.currentTarget) props.onClose();
    downOnScrim = false;
  };

  // Esc closes wherever the keyboard is, even after focus left every field:
  // a window listener, live only while this is mounted (open). Not while an
  // input method is composing: its Esc cancels the candidate.
  const onWindowKey = (event: KeyboardEvent) => {
    if (event.key === "Escape" && !event.isComposing && !event.defaultPrevented) {
      event.preventDefault();
      props.onClose();
    }
  };
  window.addEventListener("keydown", onWindowKey);
  onCleanup(() => window.removeEventListener("keydown", onWindowKey));

  // Tab and Shift+Tab cycle inside the dialog. The shell behind it is inert,
  // but a webview can still hand focus to <body> past the last field.
  let form!: HTMLFormElement;
  const onFormKeyDown = (event: KeyboardEvent) => {
    if (event.key !== "Tab") return;
    const focusable = [
      ...form.querySelectorAll<HTMLElement>(
        "button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), summary",
      ),
    ];
    if (focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  };

  return (
    // Esc is the window's (`main.tsx`), so it works wherever the keyboard is.
    <div class="composer-backdrop" onMouseDown={onScrimDown} onClick={onScrimClick}>
      <form
        ref={form}
        class="composer"
        role="dialog"
        aria-modal="true"
        aria-label="New worktree"
        onKeyDown={onFormKeyDown}
        onSubmit={submit}
      >
        <h2 class="composer-title">New worktree</h2>

        <label class="composer-field">
          <span class="composer-label">Project</span>
          <select
            class="composer-project"
            aria-invalid={invalid("projectPath") ? "true" : undefined}
            disabled={creating()}
            value={fields().projectPath}
            onChange={(event) => {
              setAddError(null);
              set("projectPath", event.currentTarget.value);
            }}
          >
            {/* `selected` per option, not only the select's value: a refetched
                list replaces the options, and a replaced option would
                otherwise drop the choice back to the first row. */}
            {/* Without it a select with no pick shows its first row, which
                is not what Create would send. */}
            <Show when={fields().projectPath === "" && projects().length > 0}>
              <option value="" selected disabled>
                Choose a project
              </option>
            </Show>
            <For each={projects()}>
              {(project) => (
                <option value={project.path} selected={project.path === fields().projectPath}>
                  {project.name}
                </option>
              )}
            </For>
          </select>
          <Show when={errorFor("projectPath")}>{(message) => <p class="composer-error">{message()}</p>}</Show>
          <Show when={addError()}>{(message) => <p class="composer-error composer-add-error">{message()}</p>}</Show>
          {/* Nothing to pick, once the host has answered: say how to register
              a folder rather than leaving only "Choose a project." */}
          <Show when={empty()}>
            <div class="composer-hint composer-empty-projects">
              <p>
                No projects yet. Add one repository with Add folder, or add your repositories' folder to
                workspace_defaults.workspace_scan_paths and reopen. For example (this replaces the whole list, so
                include any folder already in it):{" "}
                <code>{REGISTER_FOLDER_COMMAND}</code>
              </p>
              <button
                type="button"
                class="composer-add-folder"
                disabled={creating() || adding()}
                onClick={onAddProject}
              >
                Add folder…
              </button>
            </div>
          </Show>
        </label>
        {/* The empty state carries its own Add folder: one add button at a time. */}
        <Show when={!empty()}>
          <button
            type="button"
            class="composer-add-project"
            disabled={creating() || adding()}
            onClick={onAddProject}
          >
            Add project…
          </button>
        </Show>

        <label class="composer-field">
          <span class="composer-label">Name</span>
          <input
            type="text"
            class="composer-name"
            placeholder="What is this for?"
            maxlength="200"
            autofocus
            ref={(element) => queueMicrotask(() => element.focus())}
            disabled={creating()}
            value={fields().name}
            onInput={(event) => set("name", event.currentTarget.value)}
          />
          {/* Read-only: typing here picks the branch, it does not let a
              person type an invalid one directly (that is Advanced's job). */}
          <Show when={preview()}>{(branch) => <p class="composer-hint">Branch: {branch()}</p>}</Show>
          <Show when={errorFor("name")}>{(message) => <p class="composer-error">{message()}</p>}</Show>
        </label>

        <div class="composer-row">
          <div class="composer-field">
            <span class="composer-label">Agent</span>
            <div class="composer-agents" role="group" aria-label="Agent">
              <For each={SPAWN_AGENTS}>
                {(agent) => (
                  <button
                    type="button"
                    class="composer-agent"
                    classList={{ selected: fields().agent === agent.id }}
                    aria-pressed={fields().agent === agent.id}
                    disabled={creating()}
                    onClick={() => set("agent", agent.id)}
                  >
                    {agent.label}
                  </button>
                )}
              </For>
            </div>
          </div>
          <label class="composer-field">
            <span class="composer-label">Model</span>
            <input
              type="text"
              class="composer-model"
              placeholder="Default"
              maxlength="200"
              aria-invalid={invalid("model") ? "true" : undefined}
              disabled={creating()}
              value={fields().model}
              onInput={(event) => set("model", event.currentTarget.value)}
            />
            <Show when={errorFor("model")}>{(message) => <p class="composer-error">{message()}</p>}</Show>
          </label>
        </div>

        <label class="composer-field">
          <span class="composer-label">First prompt</span>
          <textarea
            class="composer-prompt"
            placeholder="What should the agent do first?"
            aria-invalid={invalid("prompt") ? "true" : undefined}
            disabled={creating()}
            value={fields().prompt}
            onInput={(event) => set("prompt", event.currentTarget.value)}
            onKeyDown={onPromptKeyDown}
          />
          <Show when={errorFor("prompt")}>{(message) => <p class="composer-error">{message()}</p>}</Show>
        </label>

        <details class="composer-advanced">
          <summary>Advanced</summary>
          <label class="composer-field">
            <span class="composer-label">Branch</span>
            <input
              type="text"
              class="composer-branch"
              placeholder={preview() ?? "ainb/session-<id>"}
              maxlength="200"
              aria-invalid={invalid("branch") ? "true" : undefined}
              disabled={creating()}
              value={fields().branch}
              onInput={(event) => set("branch", event.currentTarget.value)}
            />
            <Show when={errorFor("branch")}>{(message) => <p class="composer-error">{message()}</p>}</Show>
          </label>
          <label class="composer-field">
            <span class="composer-label">Create from</span>
            <input
              type="text"
              class="composer-base"
              placeholder="Repository default"
              maxlength="200"
              aria-invalid={invalid("base") ? "true" : undefined}
              disabled={creating()}
              value={fields().base}
              onInput={(event) => set("base", event.currentTarget.value)}
            />
            <Show when={errorFor("base")}>{(message) => <p class="composer-error">{message()}</p>}</Show>
          </label>
        </details>

        <Show when={failure()}>
          {(message) => (
            <p class="composer-failure" role="alert">
              {message()}
            </p>
          )}
        </Show>

        <div class="composer-footer">
          <button type="button" class="composer-cancel" onClick={props.onClose}>
            Cancel
          </button>
          <button type="submit" class="composer-create" disabled={!canSubmit()}>
            <Show when={busy()} fallback="Create">
              <span class="spinner" aria-hidden="true" />
              Creating worktree
            </Show>
          </button>
        </div>
      </form>
    </div>
  );
}
