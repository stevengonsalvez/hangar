import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import type { SessionsView_Serialize } from "../../../ainb-app/bindings/AppState";
import {
  branchPreview,
  initialFields,
  projectChoices,
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

  const projects = createMemo(() => projectChoices(props.sessions));
  const preview = createMemo(() => branchPreview(fields().name));

  const submit = (event: Event) => {
    event.preventDefault();
    if (canSubmit()) props.onSubmit(fields());
  };

  // Mod+Enter submits from the prompt textarea, where a plain Enter is a
  // newline. Every single-line field submits on a plain Enter for free: the
  // platform's own form submission, not reimplemented here.
  const onPromptKeyDown = (event: KeyboardEvent) => {
    if ((event.metaKey || event.ctrlKey) && event.key === "Enter") submit(event);
  };

  return (
    // Esc bubbles here from any field; a click on the scrim (not the panel,
    // which stops it) closes the same way, matching the palette's overlay.
    <div class="composer-backdrop" onClick={props.onClose} onKeyDown={(event) => event.key === "Escape" && props.onClose()}>
      <form
        class="composer"
        role="dialog"
        aria-modal="true"
        aria-label="New worktree"
        onClick={(event) => event.stopPropagation()}
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
        </label>

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
