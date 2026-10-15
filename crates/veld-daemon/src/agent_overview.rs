//! What the daemon knows about coding agents, for the bare `veld` overview.
//!
//! A hook's report (`POST /api/pty/sessions/{id}/agent-state`) used to be fanned
//! out to the IDE's websocket and forgotten: only a window that was connected at
//! that moment ever learned it. The CLI has no websocket, so the overview needs a
//! record the daemon itself keeps. This is that record: one entry per terminal
//! session this daemon holds, replaced by each accepted report and dropped when the
//! session ends, is released, or its agent reports `done`.
//!
//! In memory on purpose: a state is only true while the agent is, and a persisted
//! "working" would outlive an agent that died with the old daemon. The cost is that
//! a restart forgets — and an agent that is *blocked* sends no hook until the human
//! answers it, so after a restart the overview cannot know it is waiting. That is
//! why the overview carries `started_at`: an empty list means "none reported since
//! then", and the CLI says so instead of a confident "none".

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

use chrono::{DateTime, SecondsFormat, Utc};
use veld_core::agent::{AgentTool, State};
use veld_engine::api::{Attention, Overview, OverviewAgent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentSnapshot {
    pub session_id: String,
    pub worktree_id: i64,
    pub tool: AgentTool,
    pub state: State,
    pub held: bool,
    /// When `state`/`held` last *changed*, not when the last report arrived: a hook
    /// that repeats `working` on every tool call must not make a long-running agent
    /// look as if it just started.
    pub since: DateTime<Utc>,
}

static AGENTS: LazyLock<Mutex<HashMap<String, AgentSnapshot>>> = LazyLock::new(Default::default);

/// When this daemon started remembering. Forced by [`mark_start`] when the routes
/// are built, so it is the daemon's start rather than the first overview request.
static STARTED_AT: LazyLock<DateTime<Utc>> = LazyLock::new(Utc::now);

pub(crate) fn mark_start() {
    LazyLock::force(&STARTED_AT);
}

/// Record an accepted report. Called by the handler **while it holds the session
/// registry's lock** and only for a session still in it, so a report racing the
/// session's end cannot land after [`forget`] and leave a row nothing removes.
pub(crate) fn record(
    session_id: &str,
    worktree_id: i64,
    tool: AgentTool,
    state: State,
    held: bool,
) {
    record_at(
        &mut AGENTS.lock().expect("agents poisoned"),
        session_id,
        worktree_id,
        tool,
        state,
        held,
        Utc::now(),
    );
}

fn record_at(
    map: &mut HashMap<String, AgentSnapshot>,
    session_id: &str,
    worktree_id: i64,
    tool: AgentTool,
    state: State,
    held: bool,
    now: DateTime<Utc>,
) {
    let state = match (state, map.get(session_id).map(|p| p.state)) {
        // "Nothing to report" touches no state, here as in the inbox.
        (State::Unknown, _) => return,
        // The agent's session ended; its shell may live on, but there is no agent.
        (State::Done, _) => {
            map.remove(session_id);
            return;
        }
        // Claude's idle reminder arrives during a delegated wait too, and does not
        // end it (see `State::Settled`): the session is still working.
        (State::Settled, Some(State::Delegated)) => State::Delegated,
        // The same reminder after a plain turn end says nothing new: the agent was
        // already the human's turn, and resetting `since` would understate how long
        // it has waited.
        (State::Settled, Some(State::Idle)) => State::Idle,
        // With no agent on record — it reported `done`, or never reported — a
        // reminder is a late one from an agent that has quit. The IDE inbox drops
        // it for the same reason; recording it would list the quit agent as
        // "your turn" for as long as its shell lives.
        (State::Settled, None) => return,
        (state, _) => state,
    };
    let since = match map.get(session_id) {
        Some(prev) if prev.state == state && prev.held == held => prev.since,
        _ => now,
    };
    map.insert(
        session_id.to_owned(),
        AgentSnapshot {
            session_id: session_id.to_owned(),
            worktree_id,
            tool,
            state,
            held,
            since,
        },
    );
}

/// Drop a session's entry: its terminal ended, or this daemon let go of it. Without
/// this the overview would list an agent whose terminal is gone, forever.
pub(crate) fn forget(session_id: &str) {
    AGENTS.lock().expect("agents poisoned").remove(session_id);
}

/// Every recorded agent whose shell is still running, needing the human first.
///
/// `live` is the set of sessions whose shell has not exited. An exited shell's
/// session lingers in the registry for a grace period, and an agent killed without
/// a hook would otherwise stay "working" in it until then.
pub(crate) fn snapshot(live: &HashSet<String>) -> Vec<AgentSnapshot> {
    sorted(
        AGENTS
            .lock()
            .expect("agents poisoned")
            .values()
            .filter(|a| live.contains(&a.session_id))
            .cloned()
            .collect(),
    )
}

/// In [`Attention`] order; within a group longest-waiting first, with the session
/// id as a tiebreak so the order is stable.
fn sorted(mut v: Vec<AgentSnapshot>) -> Vec<AgentSnapshot> {
    v.sort_by(|a, b| {
        (Attention::of(a.state), a.since, &a.session_id).cmp(&(
            Attention::of(b.state),
            b.since,
            &b.session_id,
        ))
    });
    v
}

/// Build the overview. Synchronous (SQLite); the route runs it on the blocking pool,
/// after reading `live` from the session registry.
pub(crate) fn overview(live: HashSet<String>) -> Result<Overview, String> {
    let runs = veld_engine::query::running_runs()?;
    // The agents half degrades to unnamed rows rather than failing the whole
    // overview: a daemon that cannot open the database for a worktree lookup can
    // still say which agents need the human.
    let db = veld_core::db::Db::open().ok();
    let mut names: HashMap<i64, (Option<String>, Option<String>)> = HashMap::new();
    let agents = snapshot(&live)
        .into_iter()
        .map(|a| {
            let (worktree_name, project_name) = names
                .entry(a.worktree_id)
                .or_insert_with(|| {
                    db.as_ref()
                        .map(|db| names_of(db, a.worktree_id))
                        .unwrap_or_default()
                })
                .clone();
            OverviewAgent {
                session_id: a.session_id,
                worktree_id: a.worktree_id,
                worktree_name,
                project_name,
                tool: a.tool.as_str().to_owned(),
                state: a.state.as_str().to_owned(),
                attention: Attention::of(a.state),
                held: a.held,
                since: a.since.to_rfc3339_opts(SecondsFormat::Secs, true),
            }
        })
        .collect();
    Ok(Overview {
        version: veld_core::VERSION.to_owned(),
        started_at: STARTED_AT.to_rfc3339_opts(SecondsFormat::Secs, true),
        runs,
        agents,
    })
}

fn names_of(db: &veld_core::db::Db, worktree_id: i64) -> (Option<String>, Option<String>) {
    let Ok(Some(wt)) = db.get_worktree(worktree_id) else {
        return (None, None);
    };
    let title = if wt.display_name.is_empty() {
        wt.alias.clone()
    } else {
        wt.display_name.clone()
    };
    let project = db
        .get_repo(std::path::Path::new(&wt.repo_root))
        .ok()
        .flatten()
        .map(|r| r.name);
    (Some(title), project)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    #[test]
    fn a_report_replaces_the_entry_and_keeps_since_while_nothing_changed() {
        let mut m = HashMap::new();
        record_at(
            &mut m,
            "s",
            1,
            AgentTool::Claude,
            State::Working,
            false,
            at(10),
        );
        record_at(
            &mut m,
            "s",
            1,
            AgentTool::Claude,
            State::Working,
            false,
            at(20),
        );
        assert_eq!(m["s"].since, at(10), "a repeated state is not a change");
        record_at(
            &mut m,
            "s",
            1,
            AgentTool::Claude,
            State::Blocked,
            false,
            at(30),
        );
        assert_eq!(m["s"].state, State::Blocked);
        assert_eq!(m["s"].since, at(30));
        // `held` flipping is a change too: the same `blocked` now stays.
        record_at(
            &mut m,
            "s",
            1,
            AgentTool::Claude,
            State::Blocked,
            true,
            at(40),
        );
        assert_eq!(m["s"].since, at(40));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn the_shared_store_records_and_forgets() {
        record(
            "agent-overview-test",
            7,
            AgentTool::Claude,
            State::Blocked,
            true,
        );
        let live: HashSet<String> = ["agent-overview-test".to_owned()].into();
        let got = snapshot(&live);
        let ours = got.iter().find(|a| a.session_id == "agent-overview-test");
        assert_eq!(ours.map(|a| (a.worktree_id, a.held)), Some((7, true)));
        forget("agent-overview-test");
        assert!(
            snapshot(&live)
                .iter()
                .all(|a| a.session_id != "agent-overview-test")
        );
    }

    #[test]
    fn the_human_comes_first() {
        let mut m = HashMap::new();
        for (id, state, t) in [
            ("done", State::Done, 1),
            ("work", State::Working, 2),
            ("idle", State::Idle, 3),
            ("late", State::Blocked, 9),
            ("early", State::Blocked, 4),
            ("ready", State::Ready, 5),
        ] {
            record_at(&mut m, id, 1, AgentTool::Claude, state, false, at(t));
        }
        let order: Vec<_> = sorted(m.into_values().collect())
            .into_iter()
            .map(|a| a.session_id)
            .collect();
        // `done` is not an agent any more; `ready` is the human's turn like `idle`.
        assert_eq!(order, ["early", "late", "idle", "ready", "work"]);
    }

    #[test]
    fn done_drops_the_row_unknown_touches_nothing_and_settled_keeps_a_delegated_wait() {
        let mut m = HashMap::new();
        let rec = |m: &mut HashMap<_, _>, state, t| {
            record_at(m, "s", 1, AgentTool::Claude, state, false, at(t))
        };
        rec(&mut m, State::Delegated, 1);
        rec(&mut m, State::Settled, 2);
        assert_eq!(m["s"].state, State::Delegated);
        assert_eq!(m["s"].since, at(1), "the wait did not change");
        rec(&mut m, State::Unknown, 3);
        assert_eq!(m["s"].state, State::Delegated);
        rec(&mut m, State::Done, 4);
        assert!(m.is_empty());
        rec(&mut m, State::Unknown, 5);
        assert!(m.is_empty(), "unknown never creates a row");
        rec(&mut m, State::Settled, 6);
        assert!(
            m.is_empty(),
            "a late reminder after `done` does not resurrect the agent"
        );
        rec(&mut m, State::Idle, 7);
        rec(&mut m, State::Settled, 8);
        assert_eq!(
            (m["s"].state, m["s"].since),
            (State::Idle, at(7)),
            "still the same wait"
        );
    }

    #[test]
    fn an_exited_shell_drops_out_of_the_snapshot() {
        record(
            "agent-overview-exited",
            3,
            AgentTool::Claude,
            State::Working,
            false,
        );
        assert!(
            snapshot(&HashSet::new())
                .iter()
                .all(|a| a.session_id != "agent-overview-exited")
        );
        forget("agent-overview-exited");
    }
}
