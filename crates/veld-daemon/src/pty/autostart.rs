//! Starting a handed-off worktree's agent with no window showing it.
//!
//! `veld ide worktree new --prompt` used to leave the agent pane for the first
//! window that showed the worktree, so nothing ran until the human clicked the
//! row. With `worktree.autostartHandoffs` on (the default) the daemon starts it
//! itself, the moment the hand-off is recorded:
//!
//! 1. The hand-off row is marked `started` **before** anything spawns. A window
//!    that shows the worktree from then on attaches to the session the row names
//!    instead of reviewing or typing the prompt, and the detach reaper leaves that
//!    session alone however long nobody opens it (`Db::started_handoff_sessions`).
//! 2. The session is spawned through [`super::prepare_ticket`] and
//!    [`super::obtain_session`] — the same checks, environment and launch-prompt
//!    routing a window's ticket gets. It runs in its holder like any detached
//!    terminal.
//! 3. The prompt goes in the way a window's would: handed over at launch where the
//!    agent takes one (Claude), otherwise typed once the agent opens an input.
//!    [`deliver`] is `promptDelivery.ts`'s sequence without a browser — the same
//!    gate (DECSET 2004), the same settle, a bracketed paste, then `\r` once the
//!    paste has been echoed.
//!
//! The first window that shows the worktree seeds a tab under the session id and
//! attaches; that attach finds the session live and retires the row
//! (`HandoffStep::RetireNow`), and from then on it is an ordinary pane.
//!
//! **If the prompt cannot be delivered** — the agent never opens an input, or it
//! exits — the session is hung up and the row goes back to waiting, so the next
//! window to show the worktree starts it the old way and the human sees the
//! prompt. An agent that silently never got its task is the outcome this avoids.

use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use portable_pty::PtySize;
use tokio::sync::broadcast;
use tracing::{info, warn};

use super::{PaneMode, Prepared, Session, TicketRequest, wire};

/// How long the agent has to open an input. Longer than the window's 25 s
/// (`PROMPT_WAIT_MS`): a first run in a fresh checkout can spend a while on a
/// version check or an auth refresh, and nobody is watching a spinner here.
const INPUT_WAIT: Duration = Duration::from_secs(90);

/// Between the gate opening and the paste — `PROMPT_SETTLE_MS` in the UI.
const SETTLE: Duration = Duration::from_millis(300);

/// The paste counts as shown after this long without output — `PROMPT_ECHO_QUIET_MS`.
const ECHO_QUIET: Duration = Duration::from_millis(250);

/// And the `\r` goes anyway after this long — `PROMPT_ECHO_CAP_MS`.
const ECHO_CAP: Duration = Duration::from_millis(5_000);

/// The size the headless session starts at. The first window to attach resizes it
/// to its own, as every attach does.
const SIZE: PtySize = PtySize {
    cols: 120,
    rows: 40,
    pixel_width: 0,
    pixel_height: 0,
};

/// Hand-off sessions this daemon is starting right now, from the mark until the
/// prompt is in. While one is here its session can be not-yet-live and still
/// `started`, which is what `GET /api/worktrees/{id}/handoff` must not mistake for
/// a launched agent that has since died.
static IN_PROGRESS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

/// Whether the daemon is still starting this hand-off session.
pub(crate) fn in_progress(session_id: &str) -> bool {
    IN_PROGRESS
        .lock()
        .expect("autostart set poisoned")
        .contains(session_id)
}

const BRACKETED_ON: &[u8] = b"\x1b[?2004h";
const BRACKETED_OFF: &[u8] = b"\x1b[?2004l";

/// Start a handed-off worktree's agent pane now, with `prompt` as its first
/// message. `pane` is the declared pane to run, already resolved against the new
/// checkout. Logs and gives the hand-off back to the windows on any failure; never
/// panics the caller, which has long since answered the CLI.
pub(crate) async fn autostart_handoff(
    worktree_id: i64,
    session_id: String,
    pane: String,
    prompt: String,
) {
    IN_PROGRESS
        .lock()
        .expect("autostart set poisoned")
        .insert(session_id.clone());
    let outcome = run(worktree_id, &session_id, &pane, &prompt).await;
    IN_PROGRESS
        .lock()
        .expect("autostart set poisoned")
        .remove(&session_id);
    match outcome {
        Ok(()) => info!(
            session = %session_id,
            worktree = worktree_id,
            pane = %pane,
            "started a handed-off agent with no window open on it"
        ),
        Err(why) => warn!(
            session = %session_id,
            worktree = worktree_id,
            "handed-off agent not started in the background, it waits for a window: {why}"
        ),
    }
}

async fn run(worktree_id: i64, session_id: &str, pane: &str, prompt: &str) -> Result<(), String> {
    // Marked first: see the module note. `false` means the row is gone or names
    // another session — a window got there first, or the hand-off was replaced.
    let marked = set_started(worktree_id, session_id, Some(pane)).await?;
    if !marked {
        return Err("the hand-off changed before it could start".to_owned());
    }
    let started = spawn(worktree_id, session_id, pane, prompt).await;
    let (session, at_launch) = match started {
        Ok(started) => started,
        Err(why) => {
            give_back(worktree_id, session_id, false).await;
            return Err(why);
        }
    };
    if let Err(why) = deliver(&session, prompt, at_launch).await {
        give_back(worktree_id, session_id, true).await;
        return Err(why);
    }
    Ok(())
}

/// Spawn the session through the window's own path; returns it and whether the
/// launch was handed the prompt.
async fn spawn(
    worktree_id: i64,
    session_id: &str,
    pane: &str,
    prompt: &str,
) -> Result<(Arc<Session>, bool), String> {
    let Prepared {
        ticket,
        resumed,
        prompt_at_launch,
    } = super::prepare_ticket(TicketRequest {
        worktree_id,
        session_id: session_id.to_owned(),
        pane: Some(pane.to_owned()),
        mode: Some(PaneMode::Fresh),
        session_token: None,
        prompt: Some(prompt.to_owned()),
    })
    .await
    .map_err(|(status, body)| format!("{status}: {}", body.0))?;
    if resumed {
        // Something already runs under the hand-off's id — a window that started
        // it in the instant before the mark. That pane is the agent; leave it.
        return Err("a session already runs under the hand-off's id".to_owned());
    }
    let (session, _) = super::obtain_session(&ticket, SIZE)
        .await
        .map_err(|e| match e {
            super::SessionError::AtCapacity => "too many terminal sessions are open".to_owned(),
            super::SessionError::Spawn(e) | super::SessionError::Unreachable(e) => e.to_string(),
            super::SessionError::Starting => {
                "another start of this session never finished".to_owned()
            }
        })?;
    Ok((session, prompt_at_launch))
}

/// Put the hand-off back in the windows' hands: hang up the session if one was
/// spawned, and clear `started` so the next window to show the worktree starts it
/// with the prompt. Only while the row is still ours — a window that attached in
/// the meantime retired it, and that pane is the user's now.
async fn give_back(worktree_id: i64, session_id: &str, spawned: bool) {
    match set_started(worktree_id, session_id, None).await {
        Ok(true) => {
            if spawned {
                super::end_session(session_id, "handed-off agent never took its prompt").await;
            }
        }
        Ok(false) => {}
        Err(e) => warn!(session = %session_id, "hand-off not given back to the windows: {e}"),
    }
}

/// Mark the hand-off started under `pane`, or unmark it with `None`. Whether the
/// row was there for exactly this worktree and session.
async fn set_started(
    worktree_id: i64,
    session_id: &str,
    pane: Option<&str>,
) -> Result<bool, String> {
    let (session_id, pane) = (session_id.to_owned(), pane.map(str::to_owned));
    crate::offload::blocking(move || {
        let db = super::open_db().map_err(|e| format!("database unavailable: {e}"))?;
        match pane {
            Some(pane) => db.mark_handoff_started(worktree_id, &session_id, &pane),
            None => db.unmark_handoff_started(worktree_id, &session_id),
        }
        .map_err(|e| e.to_string())
    })
    .await
}

/// Watches a session's output for the bracketed-paste mode, across chunk
/// boundaries — the escape can arrive split over two reads.
struct PasteMode {
    on: bool,
    tail: Vec<u8>,
}

impl PasteMode {
    fn new(seen: &[u8]) -> Self {
        let mut mode = Self {
            on: false,
            tail: Vec::new(),
        };
        mode.feed(seen);
        mode
    }

    fn feed(&mut self, chunk: &[u8]) {
        let mut buf = std::mem::take(&mut self.tail);
        buf.extend_from_slice(chunk);
        let last = |needle: &[u8]| buf.windows(needle.len()).rposition(|w| w == needle);
        match (last(BRACKETED_ON), last(BRACKETED_OFF)) {
            (Some(on), Some(off)) => self.on = on > off,
            (Some(_), None) => self.on = true,
            (None, Some(_)) => self.on = false,
            (None, None) => {}
        }
        let keep = buf.len().saturating_sub(BRACKETED_ON.len() - 1);
        self.tail = buf.split_off(keep);
    }
}

/// What the output stream said while waiting.
enum Heard {
    Output,
    Quiet,
    Gone,
}

async fn listen(
    rx: &mut broadcast::Receiver<axum::body::Bytes>,
    mode: &mut PasteMode,
    wait: Duration,
) -> Heard {
    match tokio::time::timeout(wait, rx.recv()).await {
        Ok(Ok(chunk)) => {
            mode.feed(&chunk);
            Heard::Output
        }
        // Fell behind: the bytes in between are lost to this reader, but the
        // next chunk still says where the mode stands as soon as it changes.
        Ok(Err(broadcast::error::RecvError::Lagged(_))) => Heard::Output,
        Ok(Err(broadcast::error::RecvError::Closed)) => Heard::Gone,
        Err(_) => Heard::Quiet,
    }
}

/// Hand the agent its prompt once it is reading input — `deliverQueuedPrompt` in
/// `panes/promptDelivery.ts`, without the browser. See that function for why the
/// gate is bracketed paste and never a delay: a pane is
/// `<shell> -l -i -c '<command>'`, and text written before the agent has the
/// terminal is read by whatever does.
async fn deliver(session: &Arc<Session>, prompt: &str, at_launch: bool) -> Result<(), String> {
    let (mut rx, seen) = {
        let sb = session.scrollback.lock().expect("scrollback poisoned");
        (session.output.subscribe(), sb.snapshot())
    };
    let mut mode = PasteMode::new(&seen);
    let deadline = Instant::now() + INPUT_WAIT;
    while !mode.on {
        if session.exited().is_some() {
            return Err("the agent exited before it opened an input".to_owned());
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!(
                "the agent did not open an input within {}s",
                INPUT_WAIT.as_secs()
            ));
        }
        if let Heard::Gone = listen(&mut rx, &mut mode, left.min(Duration::from_secs(1))).await {
            return Err("the session ended before the agent opened an input".to_owned());
        }
    }
    // Settle, still reading, so a mode the agent drops again is seen.
    let settle_until = Instant::now() + SETTLE;
    loop {
        let left = settle_until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        if let Heard::Gone = listen(&mut rx, &mut mode, left).await {
            return Err("the session ended before the prompt was sent".to_owned());
        }
    }
    // The window's claim: asked once the input is up, which is after the agent's
    // wrapper ran. A file the wrapper (or the daemon, for Claude on the mod) took
    // is a prompt the agent already has.
    if at_launch && !claim_launch_prompt(&session.id).await {
        return Ok(());
    }
    if !mode.on || session.exited().is_some() {
        return Err("the agent's input closed before the prompt was sent".to_owned());
    }
    send(session, paste_frame(prompt)).await?;
    // Wait for the paste to show — the program's redraw, then a quiet spell — so
    // the `\r` cannot submit a message the agent is still reading in.
    let cap = Instant::now() + ECHO_CAP;
    let mut heard = false;
    loop {
        let left = cap.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match listen(
            &mut rx,
            &mut mode,
            if heard { ECHO_QUIET.min(left) } else { left },
        )
        .await
        {
            Heard::Output => heard = true,
            Heard::Quiet => break,
            Heard::Gone => return Err("the session ended while the prompt was pasted".to_owned()),
        }
    }
    // Re-read the gate, as the window does before every write: a `\r` into
    // whatever replaced the agent's input would be a keystroke nobody asked for.
    if !mode.on || session.exited().is_some() {
        return Err("the agent's input closed before the prompt was submitted".to_owned());
    }
    send(session, b"\r".to_vec()).await
}

/// Take the staged launch prompt back, as `launch-prompt/take` does for a window:
/// true when it is still there and the daemon now owes it, false when the agent
/// took it at launch (or there never was one).
async fn claim_launch_prompt(session_id: &str) -> bool {
    let staged = super::LAUNCH_PROMPTS
        .lock()
        .expect("launch prompts poisoned")
        .remove(session_id);
    let Some(path) = staged else {
        return false;
    };
    let taken =
        tokio::task::spawn_blocking(move || veld_core::agent::take_launch_prompt(&path)).await;
    // Unreadable counts as owed, for the window's reason: the wrapper takes the
    // file by renaming it away, so if this side could not, neither could it.
    !matches!(taken, Ok(Ok(None)))
}

/// The prompt as one bracketed paste, the way `Terminal.paste` sends it: every
/// line ending a `\r`, wrapped in `ESC[200~`/`ESC[201~`. **Every `ESC` is taken
/// out**: an agent wrote this prompt, and an `ESC[201~` inside it would end the
/// paste early and let the rest arrive as keystrokes — a `\r` among them submits.
fn paste_frame(prompt: &str) -> Vec<u8> {
    let text = prompt
        .replace("\r\n", "\r")
        .replace('\n', "\r")
        .replace('\x1b', "");
    let mut frame = b"\x1b[200~".to_vec();
    frame.extend_from_slice(text.as_bytes());
    frame.extend_from_slice(b"\x1b[201~");
    frame
}

async fn send(session: &Session, data: Vec<u8>) -> Result<(), String> {
    session
        .to_holder
        .send((wire::INPUT, data))
        .await
        .map_err(|_| "the session's holder is gone".to_owned())
}
