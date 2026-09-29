import { createEffect, onCleanup, onMount, Show } from "solid-js";
import { changesLine, DIRTY_HINT_TITLE, deleteCopy, type DeleteTarget, type PreviewState } from "./delete_dialog.ts";
import { label } from "./sessions.ts";

interface Props {
  target: DeleteTarget;
  state: PreviewState;
  onConfirm(): void;
  onClose(): void;
}

/**
 * The row menu's Delete confirmation, the way Orca's behaves (a Radix dialog
 * in `DeleteWorktreeDialog.tsx:336-423`): on a clean tree the confirm button
 * takes the keyboard so Delete then Enter confirms (`:340-351`); on a dirty
 * or uncounted one the button reads Force Delete and Cancel takes it instead.
 * Tab stays inside, Esc
 * or a press on the scrim cancels, and the keyboard goes back to where it was
 * (the row) when it closes.
 */
export function DeleteDialog(props: Props) {
  let dialog!: HTMLDivElement;
  let confirm!: HTMLButtonElement;
  let cancel!: HTMLButtonElement;
  const copy = () => deleteCopy(props.target.name, props.state);
  const changes = () => changesLine(props.state);

  // Where the keyboard was when the dialog opened: the row the menu closed
  // onto. Given back on close, or the sidebar when the row has gone. After
  // this tick, once the shell behind the dialog is no longer inert: an inert
  // row takes no focus.
  const opener = document.activeElement as HTMLElement | null;
  const sidebar = opener?.closest<HTMLElement>(".sidebar") ?? null;
  onCleanup(() => queueMicrotask(() => (opener?.isConnected ? opener : sidebar)?.focus()));

  onMount(() => dialog.focus());
  // The confirm button is disabled while the host checks. Once it can be
  // pressed, a clean delete puts the keyboard on it, as Orca does; a Force
  // Delete puts it on Cancel, so Enter never wipes work by reflex. Unless the
  // keyboard already moved on.
  createEffect(() => {
    if (copy().ready && document.activeElement === dialog) (copy().force ? cancel : confirm).focus();
  });

  const onWindowKey = (event: KeyboardEvent) => {
    if (event.key === "Escape" && !event.isComposing && !event.defaultPrevented) {
      event.preventDefault();
      props.onClose();
    }
  };
  window.addEventListener("keydown", onWindowKey);
  onCleanup(() => window.removeEventListener("keydown", onWindowKey));

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key !== "Tab") return;
    const focusable = [...dialog.querySelectorAll<HTMLElement>("button:not(:disabled)")];
    if (focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const at = document.activeElement;
    if (event.shiftKey && (at === first || at === dialog)) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && (at === last || at === dialog)) {
      event.preventDefault();
      first.focus();
    }
  };

  // A press closes only when it both started and ended on the scrim.
  let downOnScrim = false;

  return (
    <div
      class="dialog-backdrop"
      onMouseDown={(event) => (downOnScrim = event.target === event.currentTarget)}
      onClick={(event) => {
        if (downOnScrim && event.target === event.currentTarget) props.onClose();
        downOnScrim = false;
      }}
    >
      <div
        ref={dialog}
        class="delete-dialog"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="delete-dialog-title"
        aria-describedby="delete-dialog-description"
        tabIndex={-1}
        onKeyDown={onKeyDown}
      >
        <h2 id="delete-dialog-title" class="delete-dialog-title">
          {copy().title}
        </h2>
        <p id="delete-dialog-description" class="delete-dialog-description">
          {copy().before}
          <span class="delete-dialog-target">{label(copy().target)}</span>
          {copy().after}
        </p>
        <div class="delete-dialog-preview">
          <div class="delete-dialog-name">{label(props.target.name)}</div>
          <Show when={props.target.path !== ""}>
            <div class="delete-dialog-path">{props.target.path}</div>
          </Show>
          <Show when={changes()}>
            {(line) => (
              <div
                class={line().dirty ? "delete-dialog-dirty" : "delete-dialog-unchecked"}
                title={line().dirty ? DIRTY_HINT_TITLE : undefined}
              >
                {line().text}
              </div>
            )}
          </Show>
        </div>
        <Show when={props.state.kind === "failed" ? props.state.reason : null}>
          {(reason) => (
            <div class="delete-dialog-error" role="alert">
              {reason()}
            </div>
          )}
        </Show>
        <div class="delete-dialog-actions">
          <button ref={cancel} type="button" class="delete-dialog-cancel" onClick={() => props.onClose()}>
            Cancel
          </button>
          <button
            ref={confirm}
            type="button"
            class="delete-dialog-confirm"
            disabled={!copy().ready}
            onClick={() => props.onConfirm()}
          >
            {copy().confirm}
          </button>
        </div>
      </div>
    </div>
  );
}
