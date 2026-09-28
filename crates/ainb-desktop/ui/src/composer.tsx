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
} from "./composer.ts";

interface Props {
  /** The sessions frame the Project select and the default project come from. */
  sessions: SessionsView_Serialize | undefined;
  /** The in-flight request's own progress, held above this component so a
   * Cancel that only closes the overlay does not lose it (`composer.ts`). */
  state: CreateState;
  /** Submit `fields`; the caller checks `validate` again before it sends. */
  onSubmit(fields: ComposerFields): void;
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

  const errors = createMemo(() => validate(fields()));
  const errorFor = (field: ComposerFieldName) => errors().find((error) => error.field === field)?.message ?? null;
  const invalid = (field: ComposerFieldName) => errorFor(field) !== null;

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
  // project is filled then, a picked one is left alone.
  const [registered, { mutate: setRegistered }] = createResource(loadRegisteredProjects, { initialValue: [] });
  const projects = createMemo(() => projectChoices(props.sessions, registered()));
  createEffect(() => {
    const first = projects()[0]?.path;
    if (first !== undefined && untrack(fields).projectPath === "") set("projectPath", first);
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
        setRegistered((current) => [...(current ?? []).filter((known) => known.path !== project.path), project]);
        set("projectPath", project.path);
      },
      (error: unknown) => {
        setAdding(false);
        setAddError(String(error));
      },
    );
  };

  const submit = (event: Event) => {
    event.preventDefault();
    if (canSubmit()) props.onSubmit(fields());
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
            onChange={(event) => set("projectPath", event.currentTarget.value)}
          >
            <For each={projects()}>{(project) => <option value={project.path}>{project.name}</option>}</For>
          </select>
          <Show when={errorFor("projectPath")}>{(message) => <p class="composer-error">{message()}</p>}</Show>
          <Show when={addError()}>{(message) => <p class="composer-error composer-add-error">{message()}</p>}</Show>
          {/* Nothing to pick, once the host has answered: say how to register
              a folder rather than leaving only "Choose a project." */}
          <Show when={projects().length === 0 && !registered.loading}>
            <p class="composer-hint composer-empty-projects">
              No projects yet. Add your repositories' folder to workspace_defaults.workspace_scan_paths, then reopen.
              For example (this replaces the whole list, so include any folder already in it):{" "}
              <code>{REGISTER_FOLDER_COMMAND}</code>
            </p>
          </Show>
        </label>
        <button
          type="button"
          class="composer-add-project"
          disabled={creating() || adding()}
          onClick={onAddProject}
        >
          Add project…
        </button>

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
