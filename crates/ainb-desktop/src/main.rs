//! The desktop window: the embedded host, its executor and the sidecar
//! supervisor wired to the webview.
//!
//! Frames cross one in-process Tauri channel; the webview sends intents back as
//! `invoke("dispatch")`. A terminal tab's bytes cross a channel of their own,
//! with input, resize and acknowledgements as commands. Nothing here listens on
//! a port.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod menu;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ainb_app::config::AppConfig;
use ainb_app::wire::frame::{FrameBatch, HostId, Subscription};
use ainb_app::{Intent, Keymap};
use ainb_desktop::create::{CreateWorktreeArgs, CreatedWorktree};
use ainb_desktop::delete::{DeletePreview, TreeFate};
use ainb_desktop::executor::DesktopExecutor;
use ainb_desktop::host::{DesktopHost, FrameSink, agent_status_dialer};
use ainb_desktop::intent::{self, Refusal, RendererIntent, update};
use ainb_desktop::shell::Shell;
use ainb_desktop::sidecar::{Sidecar, SidecarConfig, SidecarState, SidecarView};
use ainb_desktop::terminal::{TabEvents, TabTarget, TabsView, Terminals, Tmux};
use ainb_desktop::theme::{self, Theme, ThemePick, ThemePreference};
use ainb_desktop::updater::{self, Check, Install, Phase, Settings as UpdateSettings, Updater};
use ainb_hangar_proto::agent_status::AgentState;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{Emitter, Manager};

/// The webview's frame channel, once it has subscribed.
#[derive(Clone, Default)]
struct ChannelSink(Arc<Mutex<Option<Channel<FrameBatch>>>>);

impl FrameSink for ChannelSink {
    fn send(&mut self, batch: FrameBatch) {
        let channel = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(channel) = channel.as_ref() {
            if let Err(error) = channel.send(batch) {
                tracing::warn!(%error, "frame batch not delivered to the webview");
            }
        }
    }
}

/// Tab strip changes and tab notices, as webview events.
struct WebviewTabs(tauri::AppHandle);

impl TabEvents for WebviewTabs {
    fn tabs(&self, view: TabsView) {
        if let Err(error) = self.0.emit("terminal_tabs", view) {
            tracing::warn!(%error, "terminal tabs not delivered to the webview");
        }
    }

    fn toast(&self, message: String) {
        if let Err(error) = self.0.emit("toast", message) {
            tracing::warn!(%error, "toast not delivered to the webview");
        }
    }
}

struct Window {
    shell: Shell<ChannelSink>,
    frames: ChannelSink,
    /// `None` when no tmux was found: every attach then fails with a report
    /// naming why, and the tab commands have nothing to act on.
    terminals: Option<Terminals>,
    sidecar: Sidecar,
    sidecar_config: SidecarConfig,
    /// The updater and the last check it made, which "install" acts on.
    updater: Arc<Mutex<Updater>>,
    last_check: Arc<Mutex<Option<Check>>>,
    /// The theme a person picked (`theme_set`), shared with the window's
    /// OS-theme repaint.
    theme: Arc<ThemePick>,
}

/// Drain the host's queued session-store writes before this process ends
/// (P6e).
///
/// Every path out of this shell ends the process rather than unwinding: tao's
/// run loop calls `exit` and a restart replaces the image, so no destructor
/// runs and a queued write would go with the process. The wait is bounded for
/// the whole queue, and what it leaves behind is logged rather than waited on:
/// a daemon that stopped answering must not hold the app open.
fn flush_session_store_writes(handle: &tauri::AppHandle) {
    let Some(window) = handle.try_state::<Window>() else {
        // Before `manage`, or after the state went: nothing was queued.
        return;
    };
    match window
        .shell
        .flush_session_store_writes(ainb_app::cli::util::SESSION_STORE_FLUSH_BOUND)
    {
        Some(0) => {}
        Some(dropped) => tracing::warn!(
            dropped,
            "session-store writes were not written when the app went"
        ),
        None => tracing::warn!(
            "the shell was busy for the whole bound; queued session-store writes were not drained"
        ),
    }
}

/// What the renderer applied, for the proof harness to read from the log: the
/// sections of a batch, how many session rows the sidebar holds, how many
/// cards each board column draws, and the inbox's rows and unread count.
/// Names and counts only, never a body.
///
/// The names arrive as a `Subscription`, which deserializes from the wire
/// names and drops anything else, so the line is bounded by the sections that
/// exist and a renderer cannot name one it never applied. The columns arrive
/// as `AgentState`s, so they are bounded the same way: one per state at most.
#[tauri::command]
fn renderer_applied(
    sections: Subscription,
    sessions: usize,
    board: Vec<(AgentState, usize)>,
    inbox: (usize, i64),
) {
    let named: Vec<&str> = sections.sections().map(ainb_app::wire::section_name).collect();
    let (board, dropped) = ainb_desktop::shell::board_columns(&board);
    if dropped > 0 {
        // Said, not swallowed: a proof reading the line below would otherwise
        // take a renderer that drew no board for one that drew five columns.
        tracing::warn!(
            dropped,
            "renderer applied: the board list ran past the states"
        );
    }
    // The inbox pair is rows then unread (D3p-d): what section 16 holds in
    // the window, for the proof to read against the daemon's own count.
    let (inbox_rows, inbox_unread) = inbox;
    tracing::info!(
        sections = ?named,
        sessions,
        board = ?board,
        inbox_rows,
        inbox_unread,
        "renderer applied"
    );
}

/// The terminal's copy: put the selection on the platform clipboard.
///
/// A webview cannot reach the clipboard under this CSP, and the pane's own
/// ctrl+shift+c never leaves the PTY, so the shell does it. Bounded by the
/// same limit as typed input.
#[tauri::command]
fn clipboard_write(text: String) {
    // Checked before the platform call: an oversized selection never reaches
    // the clipboard, and a box with no display refuses it the same way.
    let bytes = text.len();
    let Some(text) = ainb_desktop::clipboard::within_limit(text) else {
        tracing::warn!(bytes, "clipboard write over 1 MiB refused");
        return;
    };
    if let Err(error) = arboard::Clipboard::new().and_then(|mut board| board.set_text(text)) {
        tracing::warn!(%error, "the selection did not reach the clipboard");
    }
}

/// The terminal's paste: the clipboard's text for the pane `key` is showing.
///
/// Answered only for a tab the window has in front of the operator, which is
/// the only caller: paste is a pane's own accelerator, so no other renderer
/// path, and no driver on a `wdio` build, reads what was last copied. Empty
/// when the clipboard holds no text, cannot be read, or holds more than a
/// pane's input limit, and an oversized clipboard says so on a toast rather
/// than pasting nothing in silence.
#[tauri::command]
fn clipboard_read(app: tauri::AppHandle, window: tauri::State<'_, Window>, key: String) -> String {
    let showing = window.terminals.as_ref().is_some_and(|terminals| terminals.showing(&key));
    if !showing {
        tracing::warn!(
            tab = key,
            "clipboard read for a tab that is not in view; refused"
        );
        return String::new();
    }
    match arboard::Clipboard::new().and_then(|mut board| board.get_text()) {
        Ok(text) => {
            let bytes = text.len();
            ainb_desktop::clipboard::within_limit(text).unwrap_or_else(|| {
                tracing::warn!(bytes, "clipboard read over 1 MiB refused");
                WebviewTabs(app)
                    .toast("what was copied is over 1 MiB; it was not pasted".to_string());
                String::new()
            })
        }
        Err(error) => {
            tracing::warn!(%error, "the clipboard was not read");
            String::new()
        }
    }
}

/// Every command the palette may offer, with whether each is active now.
#[tauri::command]
fn palette(window: tauri::State<'_, Window>) -> Vec<ainb_desktop::host::PaletteEntry> {
    window.shell.palette()
}

/// Put the reducer on the session list before the answer banner sends its
/// rows (#121): they are the session list's, and a page the reducer is on
/// would refuse them. Decided by the reducer's own screen.
#[tauri::command]
fn answer_home(window: tauri::State<'_, Window>) {
    window.shell.answer_home();
}

/// The tab strip, for the webview's first paint.
#[tauri::command]
fn terminal_tabs(window: tauri::State<'_, Window>) -> TabsView {
    window.terminals.as_ref().map_or(
        TabsView {
            tabs: Vec::new(),
            focus: None,
        },
        Terminals::view,
    )
}

/// Send the tab's output to `bytes` as raw buffers. `false` for an unknown tab.
#[tauri::command]
fn terminal_output(
    window: tauri::State<'_, Window>,
    key: String,
    bytes: Channel<InvokeResponseBody>,
) -> bool {
    window.terminals.as_ref().is_some_and(|terminals| {
        terminals.attach_output(
            &key,
            Box::new(move |chunk| bytes.send(InvokeResponseBody::Raw(chunk)).is_ok()),
        )
    })
}

/// The webview painted `bytes` of the tab's output.
#[tauri::command]
fn terminal_ack(window: tauri::State<'_, Window>, key: String, bytes: usize) {
    if let Some(terminals) = &window.terminals {
        terminals.ack(&key, bytes);
    }
}

/// The most text one input call may carry. The input queue bounds how many
/// calls wait, so this bounds the bytes they hold; a larger paste is refused.
const MAX_INPUT_BYTES: usize = ainb_desktop::clipboard::MAX_CLIPBOARD_BYTES;

/// Typed or pasted text for the tab's pane.
#[tauri::command]
fn terminal_input(window: tauri::State<'_, Window>, key: String, data: String) {
    if data.len() > MAX_INPUT_BYTES {
        tracing::warn!(
            tab = key,
            bytes = data.len(),
            "terminal input over 1 MiB refused"
        );
        return;
    }
    if let Some(terminals) = &window.terminals {
        terminals.input(&key, data.into_bytes());
    }
}

#[tauri::command]
fn terminal_resize(window: tauri::State<'_, Window>, key: String, cols: u16, rows: u16) {
    if let Some(terminals) = &window.terminals {
        terminals.resize(&key, cols, rows);
    }
}

/// The tabs the layout has on screen, exactly `keys`: the cap spares each,
/// and each may paste. `false`, changing nothing, for too many keys.
#[tauri::command]
fn terminal_visible(window: tauri::State<'_, Window>, keys: Vec<String>) -> bool {
    window.terminals.as_ref().is_some_and(|terminals| terminals.set_visible(keys))
}

/// Cmd+K: clear the tab's scrollback in tmux and redraw its client. Off the
/// main thread: it forks tmux twice, and a wedged tmux must not stall the
/// window. A failure is the toast `Terminals::clear` shows.
#[tauri::command]
async fn terminal_clear(window: tauri::State<'_, Window>, key: String) -> Result<(), String> {
    let Some(terminals) = window.terminals.clone() else {
        return Ok(());
    };
    tauri::async_runtime::spawn_blocking(move || terminals.clear(&key))
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn terminal_close(window: tauri::State<'_, Window>, key: String) {
    if let Some(terminals) = &window.terminals {
        terminals.close(&key);
    }
}

/// Create a worktree with an agent in it, then open its tab.
///
/// The daemon does the work (`worktree/create`); this command only asks and
/// attaches the tmux session it names. The sidebar picks the session up on
/// its next scan. A refusal comes back as the sentence the composer shows.
#[tauri::command]
async fn worktree_create(
    app: tauri::AppHandle,
    window: tauri::State<'_, Window>,
    args: CreateWorktreeArgs,
) -> Result<CreatedWorktree, String> {
    let client = ainb_app::fleet::bridge::daemon::surface_client(
        ainb_hangar_proto::connections::SurfaceKind::Desktop,
    )
    .map_err(|error| ainb_desktop::create::refusal_text(&error))?;
    let created = ainb_desktop::create::request(&client, args).await?;
    tracing::info!(session = %created.session_id, "window created a worktree session");
    // The session exists either way; a tab that cannot open is said out loud
    // rather than leaving the person with a closed composer and nothing new.
    let attach_problem = match (
        &window.terminals,
        uuid::Uuid::parse_str(&created.session_id),
    ) {
        (None, _) => Some("no tmux was found".to_string()),
        (Some(_), Err(error)) => Some(format!(
            "the daemon named session {:?}: {error}",
            created.session_id
        )),
        (Some(terminals), Ok(id)) => {
            let target = TabTarget::Session {
                id,
                tmux: created.tmux_session_name.clone(),
            };
            // A report back is the tab failing to attach; the reducer shows it
            // in its own words, so this does not repeat it.
            if let Some(report) = terminals.open(target) {
                window.shell.dispatch(report);
            }
            None
        }
    };
    if let Some(problem) = attach_problem {
        let message = intent::toast_text(&format!(
            "Created {} but could not open its tab: {problem}",
            created.branch
        ));
        tracing::warn!(%message, "worktree created without a tab");
        if let Err(error) = app.emit("toast", &message) {
            tracing::warn!(%error, "toast not delivered to the webview");
        }
    }
    Ok(created)
}

/// What deleting a session would remove, for the delete dialog to say before
/// anything goes. Read-only; a failure is the sentence the dialog shows.
#[tauri::command]
async fn session_delete_preview(session_id: String) -> Result<DeletePreview, String> {
    ainb_desktop::delete::preview(&session_id)
        .await
        .map_err(|error| intent::toast_text(&error))
}

/// Delete a session as the terminal's `d` then Delete does, if it still
/// removes what the dialog said (`expected`, and no more uncommitted changes
/// than `expected_changes`; `force` accepts the dirty or uncounted work the
/// dialog showed), then rescan so its row leaves the sidebar now rather than
/// on the next cadence.
#[tauri::command]
async fn session_delete(
    window: tauri::State<'_, Window>,
    session_id: String,
    expected: TreeFate,
    expected_changes: Option<u32>,
    force: bool,
) -> Result<(), String> {
    let outcome =
        ainb_desktop::delete::delete(&session_id, expected, expected_changes, force).await;
    // Rescanned either way: a removal that failed part-way may still have
    // taken the session's tmux or its row.
    window.shell.reload_workspaces();
    outcome.map_err(|error| intent::toast_text(&error))
}

/// The composer's Project select: every repository in a folder the daemon
/// creates from, sessions or not (`ainb_desktop::projects`). Read per open,
/// off the main thread, because it walks the registered folders; a failure
/// is an empty list, never an error, since the sessions frame still offers
/// the projects that have sessions.
#[tauri::command]
async fn projects_list() -> Vec<ainb_desktop::projects::RegisteredProject> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    tauri::async_runtime::spawn_blocking(move || {
        let defaults =
            AppConfig::load().map(|config| config.workspace_defaults).unwrap_or_default();
        ainb_desktop::projects::list(&home, &defaults)
    })
    .await
    .unwrap_or_else(|error| {
        tracing::warn!(%error, "projects list did not run");
        Vec::new()
    })
}

/// Whether the Add project folder picker is on screen.
static PROJECT_PICKER_OPEN: AtomicBool = AtomicBool::new(false);
/// When the picker was last cancelled; see [`PICKER_COOLDOWN`].
static PROJECT_PICKER_CANCELLED: std::sync::Mutex<Option<std::time::Instant>> =
    std::sync::Mutex::new(None);
/// How long after a cancel the picker stays shut, so a script cannot reopen
/// it the instant the person dismisses it.
const PICKER_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(2);

/// Holds [`PROJECT_PICKER_OPEN`] while the picker is up, and releases it on
/// every way out, a panic included.
struct PickerOpen;

impl PickerOpen {
    fn claim() -> Option<Self> {
        PROJECT_PICKER_OPEN
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            .then_some(Self)
    }
}

impl Drop for PickerOpen {
    fn drop(&mut self) {
        PROJECT_PICKER_OPEN.store(false, Ordering::Release);
    }
}

/// Add a project: the OS folder picker, then register what the person picked
/// so the daemon creates from it (`ainb_desktop::projects::register`).
///
/// The path never comes from the webview. A script in the page can call this
/// and can only open the picker; which folder is registered is the person's
/// answer to a dialog the OS draws. The renderer's own config edits still
/// refuse `workspace_scan_paths`, and this writes neither that key nor any
/// root: it adds one repository to the daemon's projects file, which the
/// daemon matches exactly. `Ok(None)` is a cancelled picker; `Err` is the
/// sentence the composer shows.
#[tauri::command]
async fn project_add(
    app: tauri::AppHandle,
) -> Result<Option<ainb_desktop::projects::RegisteredProject>, String> {
    use tauri_plugin_dialog::DialogExt;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("The home folder is not known, so no project can be registered.")?;
    let cancelled = *PROJECT_PICKER_CANCELLED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if cancelled.is_some_and(|at| at.elapsed() < PICKER_COOLDOWN) {
        return Err("The Add project picker was just closed; try again in a moment.".into());
    }
    // One picker at a time: a script that calls this in a loop must not
    // stack dialogs on the person.
    let Some(open) = PickerOpen::claim() else {
        return Err("The Add project picker is already open.".into());
    };
    let start = home.clone();
    // The picker blocks until answered: off the async runtime's workers.
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Add project: choose a repository folder")
            .set_directory(&start)
            .blocking_pick_folder()
    })
    .await
    .map_err(|error| format!("The folder picker did not run: {error}"))?;
    drop(open);
    let Some(picked) = picked else {
        *PROJECT_PICKER_CANCELLED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(std::time::Instant::now());
        return Ok(None);
    };
    let folder = picked
        .into_path()
        .map_err(|error| format!("The picked folder has no local path: {error}"))?;
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        ainb_desktop::projects::register(&home, &folder)
    })
    .await
    .unwrap_or_else(|error| Err(format!("Registering the folder did not run: {error}")));
    match &outcome {
        Ok(project) => tracing::info!(project = %project.path, "project registered"),
        Err(why) => tracing::info!(%why, "project not registered"),
    }
    outcome.map(Some)
}

/// The most of the sidecar log "show log" returns.
const LOG_TAIL_BYTES: u64 = 64 * 1024;

/// Attach the webview's frame channel, send it every section it names, and
/// answer with the host id those frames name: the id the mirror is pinned to,
/// `local` until the daemon names one (#1066). A later re-pin reaches the
/// webview as a `host` event before its frames. The webview owns the one
/// subscription list; unknown section names are dropped.
#[tauri::command]
fn subscribe(
    window: tauri::State<'_, Window>,
    frames: Channel<FrameBatch>,
    sections: Subscription,
) -> HostId {
    *window.frames.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(frames);
    window.shell.subscribe(sections)
}

/// Apply an intent from the webview: a key, a command, pasted text. A
/// host-authored command, or a key or name the host refuses from the window,
/// is not applied, and the answer says which row and why, for a toast.
#[tauri::command]
fn dispatch(window: tauri::State<'_, Window>, intent: RendererIntent) -> Option<Refusal> {
    // What the webview asked for and what became of it, so a reader of the log
    // can tell what the window authored and what the host actually applied: a
    // command's id, never its arguments, and never a key's chord or a text's
    // characters, which are what a person typed.
    let asked = match &intent {
        RendererIntent::Command(id, _) => id.as_str().to_string(),
        RendererIntent::Key(_) => "key".to_string(),
        RendererIntent::Text(text) => format!("text({} chars)", text.chars().count()),
    };
    let refusal = match Intent::try_from(intent) {
        Ok(intent) => window.shell.dispatch_renderer(intent),
        Err(refusal) => Some(refusal),
    };
    let outcome = if refusal.is_some() {
        "refused"
    } else {
        "dispatched"
    };
    tracing::info!(command = %asked, outcome, "renderer intent");
    refusal
}

/// What the settings page's Setup panel shows: the catalog's dependencies as
/// detected now, and the two files (#1175). A host read, never a frame, off
/// the main thread because detection probes binaries.
#[tauri::command]
async fn setup_status() -> ainb_desktop::setup::SetupView {
    tauri::async_runtime::spawn_blocking(ainb_desktop::setup::status)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "setup status did not run");
            ainb_desktop::setup::SetupView {
                dependencies: Vec::new(),
                tmux_conf_present: false,
                otel: ainb_desktop::setup::OtelView {
                    env_file_present: false,
                    settings_env_present: false,
                    alloy_installed: false,
                    alloy_running: false,
                },
            }
        })
}

/// Whether a setup confirmation dialog is on screen.
static SETUP_DIALOG_OPEN: AtomicBool = AtomicBool::new(false);

/// Ask the shell to run one of the onboarding writes that the window may
/// not run itself (#1175). The write runs only after the person answers a
/// native dialog the shell owns; a script in the page can call this command
/// and can do nothing more, because the dialog is the OS's. The outcome comes
/// back as a toast, and `true` says the write ran. Async, so the blocking
/// dialog and the install never sit on the main thread.
#[tauri::command]
async fn setup_write(app: tauri::AppHandle, write: ainb_desktop::setup::SetupWrite) -> bool {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    // The variant only: the telemetry write carries a token, and this log is
    // what `show_log` reads back into the window.
    let kind = write.kind();
    if let Err(message) = write.validate() {
        tracing::info!(write = kind, "setup write refused before the dialog");
        if let Err(error) = app.emit("toast", &message) {
            tracing::warn!(%error, "setup refusal not delivered to the webview");
        }
        return false;
    }
    // One dialog at a time: a script that calls this in a loop must not
    // stack native dialogs on the person.
    if SETUP_DIALOG_OPEN
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        tracing::info!(write = kind, "setup write refused: a dialog is open");
        if let Err(error) = app.emit("toast", "a setup dialog is already open") {
            tracing::warn!(%error, "setup refusal not delivered to the webview");
        }
        return false;
    }
    let confirmation = write.confirmation();
    let confirmed = app
        .dialog()
        .message(confirmation.body)
        .title(confirmation.title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Continue".to_string(),
            "Cancel".to_string(),
        ))
        .blocking_show();
    SETUP_DIALOG_OPEN.store(false, Ordering::Release);
    tracing::info!(write = kind, confirmed, "setup write");
    if !confirmed {
        return false;
    }
    let outcome = tauri::async_runtime::spawn_blocking(move || write.run())
        .await
        .unwrap_or_else(|error| Err(format!("the write did not run: {error}")));
    let (ran, message) = match outcome {
        Ok(message) => (true, message),
        Err(message) => (false, message),
    };
    if let Err(error) = app.emit("toast", &message) {
        tracing::warn!(%error, "setup outcome not delivered to the webview");
    }
    ran
}

/// Where the daemon connection stands, for the banner on first paint.
/// A toast to the webview (scrubbed, then cut), or a log line when the window
/// is not there.
fn handle_toast(handle: &tauri::AppHandle, text: String) {
    if let Err(error) = handle.emit("toast", intent::toast_text(&text)) {
        tracing::warn!(%error, text, "update toast not delivered to the webview");
    }
}

/// The updater's phase to the webview, for its status line.
fn emit_phase(handle: &tauri::AppHandle, phase: Phase) {
    if let Err(error) = handle.emit("update", &phase) {
        tracing::warn!(%error, "update phase not delivered to the webview");
    }
}

/// The one gate the updater's commands pass through: `id` is refused from
/// the window when it chooses where updates come from
/// (`intent::update_refusal`, the list `refused_from_webview` shares).
fn update_gate(id: &str) -> Result<(), String> {
    match intent::update_refusal(id) {
        Some(refusal) => Err(format!(
            "{} is not run from the window: {}",
            refusal.command, refusal.reason
        )),
        None => Ok(()),
    }
}

/// One line for the toast, per outcome.
fn describe_check(check: &Check) -> String {
    match check {
        Check::Off => "Updates are off in this app's settings.".to_string(),
        Check::Current { running, .. } => format!("Agents in a Box {running} is current."),
        Check::Available { version, .. } => {
            format!(
                "Agents in a Box {version} is available: Install Update and Restart from the menu."
            )
        }
        Check::Declined { reason } => format!("Update declined: {reason}"),
    }
}

/// Run the check off the main thread and remember what it found.
async fn run_update_check(handle: &tauri::AppHandle) -> Check {
    emit_phase(handle, Phase::Checking);
    let window = handle.state::<Window>();
    let updater = Arc::clone(&window.updater);
    let last = Arc::clone(&window.last_check);
    let check = tauri::async_runtime::spawn_blocking(move || {
        updater
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .check(env!("CARGO_PKG_VERSION"))
    })
    .await
    .unwrap_or_else(|error| Check::Declined {
        reason: format!("the check did not run: {error}"),
    });
    *last.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(check.clone());
    check
}

/// Stage and swap what the last check found, off the main thread.
async fn run_update_apply(handle: &tauri::AppHandle) -> Result<PathBuf, String> {
    let window = handle.state::<Window>();
    let updater = Arc::clone(&window.updater);
    let check = window
        .last_check
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .ok_or_else(|| "run Check for Updates first".to_string())?;
    let phases = handle.clone();
    let version = match &check {
        Check::Available { version, .. } => version.clone(),
        _ => String::new(),
    };
    let result = tauri::async_runtime::spawn_blocking(move || {
        let install = Install::detect().map_err(|error| error.to_string())?;
        updater
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .apply_with(&check, &install, &mut |phase| emit_phase(&phases, phase))
            .map_err(|error| format!("{error:#}"))
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|inner| inner);
    match &result {
        Ok(_) => emit_phase(handle, Phase::Installed { version }),
        Err(reason) => emit_phase(
            handle,
            Phase::Failed {
                reason: intent::toast_text(reason),
            },
        ),
    }
    result
}

async fn run_update_rollback() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| {
        let install = Install::detect().map_err(|error| error.to_string())?;
        updater::rollback(&install).map_err(|error| format!("{error:#}"))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Remove the one rollback slot, at the person's request only.
async fn run_update_discard_previous() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| {
        let install = Install::detect().map_err(|error| error.to_string())?;
        updater::clear_previous(&install).map_err(|error| format!("{error:#}"))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// The updater's check, for the webview.
#[tauri::command]
async fn update_check(app: tauri::AppHandle) -> Result<Check, String> {
    update_gate(update::CHECK)?;
    Ok(run_update_check(&app).await)
}

/// Install what the last check found and restart into it.
#[tauri::command]
async fn update_apply(app: tauri::AppHandle) -> Result<(), String> {
    update_gate(update::APPLY)?;
    run_update_apply(&app).await?;
    flush_session_store_writes(&app);
    app.restart();
}

// Rolling back and removing the previous have no command: a page script
// could force a downgrade or delete the only recovery copy. They run from
// the native menu only (`menu::UPDATE_ROLLBACK`, `menu::UPDATE_DISCARD_PREVIOUS`).

/// The updater's local settings, to read. They are set in the terminal or
/// the config file: there is no command that writes them.
#[tauri::command]
fn update_settings(window: tauri::State<'_, Window>) -> Result<UpdateSettings, String> {
    update_gate(update::SETTINGS)?;
    Ok(window
        .updater
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .settings()
        .clone())
}

/// The page's theme pick, told on every change and once at start: kept for
/// the next launch's window, and painted on this one's native chrome.
#[tauri::command]
fn theme_set(
    window: tauri::State<'_, Window>,
    webview: tauri::WebviewWindow,
    preference: ThemePreference,
) {
    if let Err(error) = window.theme.set(preference) {
        tracing::warn!(%error, "theme pick not kept for the next launch");
    }
    paint_window_theme(&webview, preference);
}

/// Create the main window already in the stored pick's theme: its appearance
/// forced before the webview exists, and its background the page's, set
/// before it is first shown. `tauri.conf.json` declares the window with
/// `create: false` so it is created here rather than before `setup`.
///
/// Under a System pick the OS can switch theme while the window is open; the
/// page follows it through `prefers-color-scheme`, and the native background
/// behind it is repainted here to match.
fn open_main_window(
    app: &tauri::AppHandle,
    theme: Arc<ThemePick>,
) -> tauri::Result<tauri::WebviewWindow> {
    let preference = theme.current();
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|window| window.label == "main")
        .cloned()
        .ok_or(tauri::Error::WindowNotFound)?;
    let webview = tauri::WebviewWindowBuilder::from_config(app, &config)?
        .theme(preference.forced().map(tauri_theme))
        .visible(false)
        .build()?;
    paint_window_theme(&webview, preference);
    let painted = webview.clone();
    webview.on_window_event(move |event| {
        if let tauri::WindowEvent::ThemeChanged(shown) = event {
            // The pick in memory, not the file: a failed write or an edit
            // behind the app's back must not move the window's paint.
            paint_window_background(&painted, theme.current(), our_theme(*shown));
        }
    });
    if config.visible {
        webview.show()?;
    }
    Ok(webview)
}

/// Hold the window's appearance to `preference` (or release it to the OS for
/// System) and set its background to the page's for the theme that resolves
/// to. A failure costs a frame of the wrong colour, never the window.
fn paint_window_theme(webview: &tauri::WebviewWindow, preference: ThemePreference) {
    if let Err(error) = webview.set_theme(preference.forced().map(tauri_theme)) {
        tracing::warn!(%error, "window appearance not set");
    }
    // Read once held or released: released (System), it is the OS's own.
    // Dark, the page's own default, when unknown.
    let shown = webview.theme().map_or(Theme::Dark, our_theme);
    paint_window_background(webview, preference, shown);
}

/// Set the window's background to `theme::window_paint`'s colour for
/// `preference` while the window shows `shown`.
fn paint_window_background(
    webview: &tauri::WebviewWindow,
    preference: ThemePreference,
    shown: Theme,
) {
    let [red, green, blue] = theme::window_paint(preference, shown);
    let color = tauri::window::Color(red, green, blue, 255);
    if let Err(error) = webview.set_background_color(Some(color)) {
        tracing::warn!(%error, "window background not set");
    }
}

/// Tauri's theme as ours; any theme it may add later reads as dark, the
/// page's own default.
fn our_theme(theme: tauri::Theme) -> Theme {
    match theme {
        tauri::Theme::Light => Theme::Light,
        _ => Theme::Dark,
    }
}

fn tauri_theme(theme: Theme) -> tauri::Theme {
    match theme {
        Theme::Light => tauri::Theme::Light,
        Theme::Dark => tauri::Theme::Dark,
    }
}

#[tauri::command]
fn sidecar_state(window: tauri::State<'_, Window>) -> SidecarView {
    window.sidecar.state().borrow().view()
}

/// The end of the sidecar log, for the degraded banner's "show log".
#[tauri::command]
fn show_log(window: tauri::State<'_, Window>) -> Option<String> {
    ainb_desktop::sidecar::log_tail(&window.sidecar_config, LOG_TAIL_BYTES)
}

/// Leave the degraded state and look for a daemon again.
#[tauri::command]
fn retry_sidecar(window: tauri::State<'_, Window>) {
    window.sidecar.retry();
}

/// The bundled daemon, beside this executable where the bundle installs
/// `bundle.externalBin`. A debug build also honours `AINB_DESKTOP_DAEMON_BIN`;
/// a release build never takes the binary it runs from the environment or from
/// `PATH`, and fails closed when it cannot place itself.
fn daemon_bin() -> Result<PathBuf, String> {
    #[cfg(debug_assertions)]
    if let Some(bin) = std::env::var_os("AINB_DESKTOP_DAEMON_BIN") {
        return Ok(PathBuf::from(bin));
    }
    let name = if cfg!(windows) {
        "ainb-hangar-daemon.exe"
    } else {
        "ainb-hangar-daemon"
    };
    let exe = std::env::current_exe()
        .map_err(|error| format!("cannot locate the desktop executable: {error}"))?;
    let dir = exe
        .parent()
        .ok_or("the desktop executable has no directory to find its daemon in")?;
    Ok(dir.join(name))
}

/// The `ainb` binary daemon lifecycle verbs run through, resolved once at
/// startup from the absolute `PATH` entries only (a relative or empty entry
/// would resolve against whatever directory the app was started in), and only
/// a file this user may execute.
fn ainb_bin() -> Option<PathBuf> {
    let name = if cfg!(windows) { "ainb.exe" } else { "ainb" };
    let found = std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|path| is_executable(path));
    match &found {
        Some(path) => tracing::info!(path = %path.display(), "daemon verbs run through ainb"),
        None => tracing::warn!("no ainb on PATH; daemon verbs will report failed"),
    }
    found
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

/// Send the shell's tracing to `<hangar home>/desktop.log`, so its warnings
/// exist somewhere once the window hides the terminal it was started from.
fn init_logging(hangar_home: &std::path::Path) {
    let path = hangar_home.join("desktop.log");
    let file = std::fs::create_dir_all(hangar_home)
        .and_then(|()| std::fs::OpenOptions::new().create(true).append(true).open(&path));
    match file {
        Ok(file) => {
            let installed = tracing_subscriber::fmt()
                .with_ansi(false)
                .with_writer(Mutex::new(file))
                .try_init();
            if let Err(error) = installed {
                eprintln!("desktop logging not installed: {error}");
            }
        }
        Err(error) => eprintln!("desktop log {} not opened: {error}", path.display()),
    }
}

// The WebDriver the journey drives serves unauthenticated commands on
// 127.0.0.1, so it exists in debug builds only. `--all-features` (the clippy
// step uses it) would otherwise reach a release binary.
#[cfg(all(feature = "wdio", not(debug_assertions)))]
compile_error!("the wdio WebDriver must never be built into a release binary");

// A release window has to serve its own frontend: without `bundled` the
// context embeds no assets and the window loads `build.devUrl`, which is a
// dev server nobody is running.
#[cfg(all(feature = "app", not(feature = "bundled"), not(debug_assertions)))]
compile_error!("a release build must carry `bundled`, or the window loads build.devUrl");

fn main() {
    // The native confirmation in front of the onboarding writes (#1175).
    let builder = tauri::Builder::default().plugin(tauri_plugin_dialog::init());
    // Only a `wdio` build carries the embedded WebDriver the journey drives.
    #[cfg(feature = "wdio")]
    let builder = builder.plugin(tauri_plugin_wdio_webdriver::init());
    builder
        .setup(|app| {
            let hangar_home = ainb_hangar_core::hangar_home()
                .ok_or("the hangar home cannot be resolved: set AINB_HANGAR_HOME")?;
            init_logging(&hangar_home);
            // First, so the window is on screen as early as before, and in
            // the right theme from its first frame.
            let theme = Arc::new(ThemePick::load(hangar_home.join(theme::THEME_FILE)));
            open_main_window(app.handle(), Arc::clone(&theme))?;
            // The desktop loads the user config itself and hands it to the
            // host, which reads nothing from disk for it.
            let config = AppConfig::load().unwrap_or_else(|error| {
                tracing::warn!(%error, "config did not load; using defaults");
                AppConfig::default()
            });
            // How often the host frames work that happened outside a
            // dispatch: the same `ui.app_tick_ms` the terminal host paces by.
            let tick = Duration::from_millis(config.ui.app_tick_ms.max(1));
            let legacy_panel = ainb_desktop::host::legacy_panel(&config);
            let frames = ChannelSink::default();
            let socket = ainb_hangar_client::socket_path_in(&hangar_home);
            let mut host = DesktopHost::new(
                config,
                Keymap::defaults(),
                // `local` until this home's daemon names its host; the sidecar
                // task below re-pins once it has.
                HostId::of_daemon(&socket),
                // Nothing is framed until the webview subscribes.
                Subscription::none(),
                frames.clone(),
            )
            // `subscribe` is a synchronous command on the main thread, so the
            // inbox reader it starts needs the app runtime handed to it.
            .on_runtime(tauri::async_runtime::handle().inner().clone());
            let daemon_bin = daemon_bin()?;
            // A swap a crash interrupted is finished before anything else,
            // so the next launch is the new version.
            match updater::repair_at_startup() {
                Ok(true) => tracing::info!("finished an interrupted update"),
                Ok(false) => {}
                Err(error) => tracing::warn!(%error, "an interrupted update was not finished"),
            }
            let updater = Arc::new(Mutex::new(Updater::new(hangar_home.clone())));
            let sidecar_config = SidecarConfig::new(hangar_home, daemon_bin);
            // Both spawn onto the app's tokio runtime, so they start inside it.
            let sidecar = tauri::async_runtime::block_on(async {
                host.start_workspace_load();
                // Section 20, the board: the reader both hosts share (#1188).
                host.start_agent_status(agent_status_dialer(), legacy_panel);
                // Section 21, the stats tab: read once the webview subscribes
                // to it, dialing as the desktop does for section 20.
                host.enable_usage(agent_status_dialer());
                Sidecar::start(sidecar_config.clone())
            });
            let mut states = sidecar.state();
            let executor = DesktopExecutor::new(ainb_bin());
            // Only an absolute tmux: a bare "tmux" would re-admit the relative
            // PATH entries `find_tmux` leaves out.
            let terminals = ainb_desktop::terminal::find_tmux().map(|tmux| {
                Terminals::new(
                    Tmux::new(tmux),
                    WebviewTabs(app.handle().clone()),
                    executor.report_sender(),
                )
            });
            if terminals.is_none() {
                tracing::warn!("no tmux found; terminal tabs will report why they cannot open");
            }
            let executor = match &terminals {
                Some(terminals) => executor.with_terminals(terminals.clone()),
                None => executor,
            };
            let shell = Shell::new(host, executor);
            // The sidebar is the session list: a row click must be in context.
            shell.open_sessions();
            app.manage(Window {
                shell,
                frames,
                terminals,
                sidecar,
                sidecar_config,
                updater,
                last_check: Arc::new(Mutex::new(None)),
                theme,
            });

            menu::install(app.handle());
            app.on_menu_event(|app, event| {
                let handle = app.clone();
                match event.id().as_ref() {
                    menu::UPDATE_CHECK => {
                        tauri::async_runtime::spawn(async move {
                            let check = run_update_check(&handle).await;
                            handle_toast(&handle, describe_check(&check));
                        });
                    }
                    menu::UPDATE_INSTALL => {
                        tauri::async_runtime::spawn(async move {
                            match run_update_apply(&handle).await {
                                Ok(path) => {
                                    tracing::info!(path = %path.display(), "update installed; restarting");
                                    flush_session_store_writes(&handle);
                                    handle.restart();
                                }
                                Err(error) => handle_toast(&handle, format!("Update not installed: {error}")),
                            }
                        });
                    }
                    menu::UPDATE_ROLLBACK => {
                        tauri::async_runtime::spawn(async move {
                            match run_update_rollback().await {
                                Ok(()) => {
                                    flush_session_store_writes(&handle);
                                    handle.restart();
                                }
                                Err(error) => handle_toast(&handle, format!("Roll back failed: {error}")),
                            }
                        });
                    }
                    menu::UPDATE_DISCARD_PREVIOUS => {
                        tauri::async_runtime::spawn(async move {
                            match run_update_discard_previous().await {
                                Ok(()) => handle_toast(&handle, "The previous version was removed.".to_string()),
                                Err(error) => handle_toast(&handle, format!("The previous version was not removed: {error}")),
                            }
                        });
                    }
                    _ => {}
                }
            });

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    let (view, connected) = {
                        let state = states.borrow_and_update();
                        (
                            state.view(),
                            matches!(*state, SidecarState::Connected { .. }),
                        )
                    };
                    // A connected sidecar has completed a hello, so the daemon
                    // may now have named its host (#1066). The webview hears the
                    // new id first, then the mirror re-pins and reframes under
                    // it; the other order would have the webview drop the
                    // reframe as another host's. Ordering dependency: the id is
                    // there to read because the sidecar publishes Connected only
                    // after its presence lease's `dial_presence`
                    // (ainb-hangar-client) completed the hello that recorded it.
                    if connected {
                        let host_id = HostId::of_daemon(&socket);
                        let window = handle.state::<Window>();
                        if host_id != window.shell.host_id() {
                            if let Err(error) = handle.emit("host", &host_id) {
                                tracing::warn!(%error, "host id not delivered to the webview");
                            }
                            window.shell.set_host(host_id);
                        }
                    }
                    if let Err(error) = handle.emit("sidecar", view) {
                        tracing::warn!(%error, "sidecar state not delivered to the webview");
                    }
                    if states.changed().await.is_err() {
                        return;
                    }
                }
            });

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut interval = tokio::time::interval(tick);
                loop {
                    interval.tick().await;
                    handle.state::<Window>().shell.tick();
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            subscribe,
            dispatch,
            sidecar_state,
            setup_status,
            setup_write,
            show_log,
            retry_sidecar,
            palette,
            renderer_applied,
            clipboard_read,
            clipboard_write,
            terminal_tabs,
            answer_home,
            terminal_output,
            terminal_ack,
            terminal_input,
            terminal_resize,
            terminal_visible,
            terminal_clear,
            terminal_close,
            worktree_create,
            session_delete_preview,
            session_delete,
            projects_list,
            project_add,
            update_check,
            update_apply,
            update_settings,
            theme_set
        ])
        .build(tauri::generate_context!())
        .unwrap_or_else(|error| {
            eprintln!("ainb desktop failed to start: {error}");
            std::process::exit(1);
        })
        // `build` and then `run`, not `run` alone: the run loop never returns,
        // so the only place the app hears that it is going is this event, and
        // a queued session-store write has to be drained there (P6e).
        .run(|handle, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                flush_session_store_writes(handle);
            }
        });
}
