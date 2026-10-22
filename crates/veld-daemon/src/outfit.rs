//! [outfit](https://outfit.oss.life.li) profiles for new worktrees.
//!
//! outfit gives one person their own copies of a few tracked files (a *profile*:
//! their `AGENTS.md`, their `.claude/skills`) without changing anything for the team.
//! A fresh `git worktree add` follows the clone's default profile; `outfit use <p>
//! --here` pins one checkout to another. Veld does that pin as part of creating a
//! worktree, so an agent started there reads the files it was meant to.
//!
//! **Whether outfit is in play is decided on the machine, never by the project
//! alone**: the binary on the user's `PATH` and at least one profile besides `base`
//! — committed *or local*. `outfit init` is not a condition: somebody who has never
//! used outfit in a team repository with committed profiles is offered them too, and
//! picking one is what sets outfit up (see [`apply`]). A local profile
//! lives in the git directory, so a project that has never heard of outfit can
//! still have one, and the person who made it must still be offered it. The
//! project's `ide.outfit` only tunes that (see [`veld_core::ide::OutfitPolicy`]).
//!
//! Every answer comes from running `outfit … --json`, the tool's own documented
//! interface, rather than from reading its state under `.git/outfit/` — which
//! outfit's own docs tell agents never to touch.

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

/// The binary veld looks for.
const OUTFIT: &str = "outfit";

/// How long a read (`status`, `ls`) may take. Reads only: they are safe to kill.
/// A `use` is never timed out — see [`apply`].
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Where outfit stands for one checkout, as every face (dialog, top bar, CLI, the
/// agent's context) needs it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OutfitState {
    /// The one field to branch on. Only [`Readiness::Ready`] offers profiles.
    pub status: Readiness,
    /// A sentence saying why it is not ready, for every status but `ready`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The command that would make it ready, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// Whether `veld ide worktree new` must name a profile here.
    pub required: bool,
    /// The project's `ide.outfit.default`, when it names an offered profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// The profile this checkout wears now, and whether it is pinned to it (`outfit
    /// use --here`) or follows the clone's default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<Current>,
    /// What a new worktree can be given: `base` first, then committed and local
    /// profiles. A worktree profile belongs to the one checkout that made it and is
    /// never offered.
    pub profiles: Vec<Profile>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    /// Profiles to choose from, and outfit can apply them.
    Ready,
    /// The project's `veld.json` says `"ide": { "outfit": false }`.
    Off,
    /// No `outfit` on the user's `PATH`.
    Absent,
    /// Installed, and `base` is the only profile: nothing to choose (set up or not).
    NoProfiles,
    /// Installed, and it did not answer. Not "absent": a create that requires a
    /// profile refuses rather than quietly skipping the choice.
    Broken,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Current {
    pub name: String,
    /// True when this checkout is pinned (`outfit use --here`); false when it
    /// follows the clone's default.
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    /// `builtin` (only `base`), `committed` or `local`.
    pub tier: String,
    /// The tracked paths it swaps; outfit has no description field. The IDE shows
    /// names only — a real profile swaps dozens of paths — so only the hand-off's
    /// refusal to an agent names any, capped.
    pub files: Vec<String>,
}

impl OutfitState {
    fn not_ready(status: Readiness, reason: impl Into<String>, fix: Option<&str>) -> Self {
        Self {
            status,
            reason: Some(reason.into()),
            fix: fix.map(str::to_owned),
            required: false,
            default: None,
            current: None,
            profiles: Vec::new(),
        }
    }

    /// Whether `name` is one this state offers.
    pub fn offers(&self, name: &str) -> bool {
        self.profiles.iter().any(|p| p.name == name)
    }

    /// The offered names, comma-separated, for a sentence.
    pub fn names(&self) -> String {
        self.profiles
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Whether "is outfit required" follows the project, or the person overrode it —
/// the `outfit.required` setting. outfit is personal, so the person it affects
/// must be able to relax (or insist on) the choice without editing a team file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredOverride {
    Project,
    Always,
    Never,
}

impl RequiredOverride {
    pub fn from_setting(value: &str) -> Self {
        match value {
            "always" => Self::Always,
            "never" => Self::Never,
            _ => Self::Project,
        }
    }

    fn resolve(self, project: bool) -> bool {
        match self {
            Self::Project => project,
            Self::Always => true,
            Self::Never => false,
        }
    }
}

/// [`probe`] with the policy read the way every face needs it: the checkout's own
/// `veld.json` (`ide.outfit`), and the person's `outfit.required` setting over it.
pub async fn state_for(checkout: &Path) -> OutfitState {
    let dir = checkout.to_owned();
    let (policy, required) = crate::offload::blocking(move || {
        let policy = veld_core::config::root_config_in(&dir)
            .as_deref()
            .and_then(super::config_cache::parse_config)
            .map(|c| c.ide_section().outfit)
            .unwrap_or_default();
        let required = super::desktop::open_desktop_db()
            .map(|db| RequiredOverride::from_setting(&db.outfit_required()))
            .unwrap_or(RequiredOverride::Project);
        (policy, required)
    })
    .await;
    probe(checkout, &policy, required).await
}

/// Ask outfit where `checkout` stands. Spawns `outfit status` and `outfit ls`, so it
/// is for a request somebody is waiting on — never the rail's poll.
pub async fn probe(
    checkout: &Path,
    policy: &veld_core::ide::OutfitPolicy,
    required: RequiredOverride,
) -> OutfitState {
    if !policy.enabled {
        return OutfitState::not_ready(
            Readiness::Off,
            "this project's veld.json turns outfit off (`\"ide\": { \"outfit\": false }`)",
            None,
        );
    }
    // **Nothing is run in a repository outfit has never touched** — most of them.
    // Without its traces there is nothing to offer, whatever binary called `outfit`
    // is on the PATH; and that binary (another tool by the same name, a hung one)
    // must not be able to fail a hand-off closed for somebody who never used outfit.
    let dir = checkout.to_owned();
    if !crate::offload::blocking(move || has_traces(&dir)).await {
        return OutfitState::not_ready(
            Readiness::NoProfiles,
            "this repository has no outfit profiles",
            None,
        );
    }
    // The directory-scoped PATH: `outfit` is the user's own install, found the way
    // their shell in this checkout would find it (AGENTS.md, daemon PATH rule).
    let path = veld_core::user_path::cached_user_path_for(checkout).await;
    if !super::pty::on_path(OUTFIT, &path) {
        return OutfitState::not_ready(Readiness::Absent, "outfit is not installed", None);
    }
    let (status, listed) = tokio::join!(
        read(checkout, &path, &["status", "--json"]),
        read(checkout, &path, &["ls", "--json"]),
    );
    let (status, listed) = match (status, listed) {
        (Ok(s), Ok(l)) => (s, l),
        (Err(e), _) | (_, Err(e)) => {
            // `required` still says what the person and project asked for: a create
            // that must choose refuses on a broken outfit instead of skipping it.
            return OutfitState {
                required: required.resolve(policy.required),
                ..OutfitState::not_ready(
                    Readiness::Broken,
                    format!("outfit did not answer: {e}"),
                    Some("outfit status"),
                )
            };
        }
    };
    interpret(&status, &listed, policy, required)
}

/// Whether outfit has ever been used in the repository `checkout` belongs to: its
/// committed profiles (`.outfit/` in the checkout) or its own directory in the git
/// common dir (local profiles, `outfit init`'s hooks). Read from the filesystem, so
/// it costs no process: `.git` is the common dir in a main checkout, and in a linked
/// one a `gitdir:` file whose directory names the common dir in `commondir`.
fn has_traces(checkout: &Path) -> bool {
    if checkout.join(".outfit").is_dir() {
        return true;
    }
    let dot_git = checkout.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        let Some(gitdir) = std::fs::read_to_string(&dot_git)
            .ok()
            .and_then(|s| s.strip_prefix("gitdir:").map(|p| p.trim().to_owned()))
        else {
            return false;
        };
        checkout.join(gitdir)
    };
    // A worktree profile lives in this checkout's own git dir.
    if git_dir.join("outfit").is_dir() {
        return true;
    }
    let common = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(rel) => git_dir.join(rel.trim()),
        Err(_) => git_dir,
    };
    common.join("outfit").is_dir()
}

/// [`probe`]'s decision, from the two answers. Separate so a test can feed it
/// outfit's real output without outfit.
fn interpret(
    status: &serde_json::Value,
    listed: &serde_json::Value,
    policy: &veld_core::ide::OutfitPolicy,
    required: RequiredOverride,
) -> OutfitState {
    let profiles: Vec<Profile> = listed["profiles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let tier = p["tier"].as_str()?;
            let name = p["name"].as_str()?;
            // `builtin` is `base`; a `worktree` profile belongs to one checkout.
            matches!(tier, "builtin" | "committed" | "local").then(|| Profile {
                name: name.to_owned(),
                tier: tier.to_owned(),
                files: p["files"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|f| f.as_str().map(str::to_owned))
                    .collect(),
            })
        })
        .collect();
    let mut profiles = profiles;
    // `base` first, whatever order outfit listed them in: it is the one choice
    // every project has, and the one that means "the team's files".
    profiles.sort_by_key(|p| p.tier != "builtin");
    if !profiles.iter().any(|p| p.tier == "builtin") {
        profiles.insert(
            0,
            Profile {
                name: "base".to_owned(),
                tier: "builtin".to_owned(),
                files: Vec::new(),
            },
        );
    }
    if profiles.len() < 2 {
        return OutfitState::not_ready(
            Readiness::NoProfiles,
            "outfit has no profile here besides `base`",
            Some("outfit new <name>"),
        );
    }
    let current = status["profile"]["name"].as_str().map(|name| Current {
        name: name.to_owned(),
        pinned: status["profile"]["source"].as_str() == Some("worktree"),
    });
    let mut state = OutfitState {
        status: Readiness::Ready,
        reason: None,
        fix: None,
        required: required.resolve(policy.required),
        default: None,
        current,
        profiles,
    };
    // A committed `default` the person does not have (a teammate's local profile
    // name, a profile since deleted) is ignored rather than preselecting nothing.
    state.default = policy.default.clone().filter(|d| state.offers(d));
    state
}

/// The outfit note waiting for each fresh agent session's first prompt, by
/// terminal session. Filled in the background when the pane launches, so the probe
/// never sits on the launch or on a hook; a prompt that arrives first just gets none.
static NOTES: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, String>>> =
    std::sync::LazyLock::new(Default::default);

/// Work out, in the background, what a fresh agent in `checkout` should be told
/// about outfit, and keep it for [`take_note`]. Nothing is kept where outfit has
/// nothing to offer.
pub fn prepare_note(session: String, checkout: String, cli: String) {
    tokio::spawn(async move {
        let state = state_for(Path::new(&checkout)).await;
        // A terminal that ended while the probe ran has nobody left to tell, and
        // `end_session` already cleared its entry — inserting now would leak it.
        if state.status != Readiness::Ready
            || super::pty::session_worktree(&session).await.is_none()
        {
            return;
        }
        let wearing = state.current.as_ref().map_or("base", |c| c.name.as_str());
        let note = veld_core::agent::outfit_context(&cli, wearing, &state.names(), state.required);
        NOTES
            .lock()
            .expect("outfit notes poisoned")
            .insert(session, note);
    });
}

/// The note for `session`, once.
pub fn take_note(session: &str) -> Option<String> {
    NOTES.lock().expect("outfit notes poisoned").remove(session)
}

/// Pin `checkout` to `profile` (`outfit use <profile> --here`).
///
/// **Sets outfit up when it is not yet.** outfit 0.14 does that inside `use`; an
/// older one refuses with the fix `outfit init`, which is then run — plain, so it
/// touches only this clone's `.git` (its `--agents`/`--team` flags write files
/// people share, and are never Veld's call) — and `use` tried once more. Picking a
/// profile is the consent; merely being offered one never sets anything up.
///
/// **Never timed out and never killed**: `use` rewrites links, sparse patterns and
/// config, and a kill halfway leaves the checkout half-switched — the same reason
/// the git write helpers do not kill on drop. outfit serialises it with its own
/// lock, so a slow one only waits.
///
/// `may_init` is whether a person (or an agent) *chose* this profile: named it, or
/// submitted the create dialog with it showing in the picker — a preselected
/// default the person saw and kept counts, which is what makes a first-time user's
/// first create just work. A profile that only arrived as a default nobody saw —
/// the project's `ide.outfit.default` on a hand-off that named none — never sets
/// outfit up; it is refused with outfit's own fix instead. And `base` never needs it: a clone outfit has not set up already
/// wears the team's files, so there is nothing to pin.
pub async fn apply(checkout: &Path, profile: &str, may_init: bool) -> Result<(), String> {
    let path = veld_core::user_path::cached_user_path_for(checkout).await;
    let out = write(checkout, &path, &["use", profile, "--here", "--json"]).await?;
    if out.status.success() {
        return Ok(());
    }
    if asks_for_init(&out.stdout) && profile == "base" {
        return Ok(());
    }
    if asks_for_init(&out.stdout) && may_init {
        let init = write(checkout, &path, &["init", "--json"]).await?;
        if !init.status.success() {
            return Err(refusal(&init.stdout, &init.stderr));
        }
        let out = write(checkout, &path, &["use", profile, "--here", "--json"]).await?;
        if out.status.success() {
            return Ok(());
        }
        return Err(refusal(&out.stdout, &out.stderr));
    }
    Err(refusal(&out.stdout, &out.stderr))
}

/// Whether a refused `use` is outfit saying it is not set up here: its fix is exactly
/// `outfit init`.
fn asks_for_init(stdout: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(stdout)
        .is_ok_and(|v| v["fix"].as_str().map(str::trim) == Some("outfit init"))
}

/// One outfit command that changes the checkout — see [`apply`] for why it is never
/// cut short.
async fn write(checkout: &Path, path: &str, args: &[&str]) -> Result<std::process::Output, String> {
    tokio::process::Command::new(OUTFIT)
        .args(args)
        .current_dir(checkout)
        .env("PATH", path)
        // The directory is the only input, as for Veld's own git (`git_raw_with_index`):
        // a daemon auto-started from inside a git hook inherits these, and outfit's git
        // would follow them into another repository.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| format!("could not run outfit: {e}"))
}

/// One sentence out of a failed outfit command, whichever of its two shapes it
/// printed: `{"error", "fix"}` for an error (exit 1, 2) or `{"refused": [..],
/// "fixes": [..]}` when a person must look (exit 3).
fn refusal(stdout: &[u8], stderr: &[u8]) -> String {
    let joined = |v: &serde_json::Value| {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .filter(|s| !s.is_empty())
    };
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(stdout) {
        if let Some(error) = v["error"].as_str() {
            return match v["fix"].as_str() {
                Some(fix) => format!("{error} (fix: {fix})"),
                None => error.to_owned(),
            };
        }
        if let Some(refused) = joined(&v["refused"]) {
            return match joined(&v["fixes"]) {
                Some(fixes) => format!("{refused} (fix: {fixes})"),
                None => refused,
            };
        }
    }
    let stderr = String::from_utf8_lossy(stderr).trim().to_owned();
    if stderr.is_empty() {
        "outfit failed without saying why".to_owned()
    } else {
        stderr
    }
}

/// One read-only outfit command's JSON answer. `status` exits 4 when the checkout is
/// out of sync and 3 when a person must look, with its JSON still on stdout — so
/// the exit code is not the test, the JSON is.
async fn read(checkout: &Path, path: &str, args: &[&str]) -> Result<serde_json::Value, String> {
    let child = tokio::process::Command::new(OUTFIT)
        .args(args)
        .current_dir(checkout)
        .env("PATH", path)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(READ_TIMEOUT, child)
        .await
        .map_err(|_| format!("`outfit {}` took longer than 10s", args[0]))?
        .map_err(|e| format!("could not run outfit: {e}"))?;
    match serde_json::from_slice::<serde_json::Value>(&out.stdout) {
        // outfit's own failure is JSON too (`{"error", "fix", ...}`, exit 1): read as
        // an answer, it would look like "no profiles" and quietly skip a required
        // choice instead of failing closed.
        Ok(v) if v.get("error").is_none() => Ok(v),
        _ => Err(refusal(&out.stdout, &out.stderr)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy() -> veld_core::ide::OutfitPolicy {
        veld_core::ide::OutfitPolicy::default()
    }

    // outfit 0.13's own output, trimmed to the fields read.
    fn listed() -> serde_json::Value {
        json!({ "active": "base", "profiles": [
            { "name": "mine", "tier": "local", "files": ["AGENTS.md"] },
            { "name": "base", "tier": "builtin", "files": [] },
            { "name": "lean", "tier": "committed", "files": [".claude/skills"] },
            { "name": "scratch", "tier": "worktree", "files": [] },
        ]})
    }

    fn status(installed: bool, source: &str) -> serde_json::Value {
        json!({ "profile": { "name": "mine", "source": source },
                "dispatcher": { "installed": installed, "ok": installed } })
    }

    #[test]
    fn ready_offers_base_first_and_never_a_worktree_profile() {
        let s = interpret(
            &status(true, "local"),
            &listed(),
            &policy(),
            RequiredOverride::Project,
        );
        assert_eq!(s.status, Readiness::Ready);
        let names: Vec<_> = s.profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["base", "mine", "lean"]);
        assert!(s.required);
        assert_eq!(
            s.current,
            Some(Current {
                name: "mine".into(),
                pinned: false
            })
        );
        let s = interpret(
            &status(true, "worktree"),
            &listed(),
            &policy(),
            RequiredOverride::Project,
        );
        assert!(s.current.unwrap().pinned);
    }

    #[test]
    fn profiles_without_init_are_offered_and_base_alone_is_nothing_to_choose() {
        // Never ran `outfit init`: still offered — picking one sets outfit up.
        let s = interpret(
            &status(false, "default"),
            &listed(),
            &policy(),
            RequiredOverride::Project,
        );
        assert_eq!(s.status, Readiness::Ready);
        assert_eq!(s.profiles.len(), 3);

        let only_base = json!({ "profiles": [{ "name": "base", "tier": "builtin" }] });
        let s = interpret(
            &status(true, "default"),
            &only_base,
            &policy(),
            RequiredOverride::Project,
        );
        assert_eq!(s.status, Readiness::NoProfiles);
    }

    #[test]
    fn the_person_overrides_the_project_and_an_unknown_default_is_ignored() {
        let mut p = policy();
        p.required = false;
        p.default = Some("teammates-own".into());
        let s = interpret(
            &status(true, "default"),
            &listed(),
            &p,
            RequiredOverride::Project,
        );
        assert!(!s.required);
        assert_eq!(s.default, None);
        let s = interpret(
            &status(true, "default"),
            &listed(),
            &p,
            RequiredOverride::Always,
        );
        assert!(s.required);
        p.default = Some("lean".into());
        let s = interpret(
            &status(true, "default"),
            &listed(),
            &p,
            RequiredOverride::Never,
        );
        assert!(!s.required);
        assert_eq!(s.default.as_deref(), Some("lean"));
    }

    #[test]
    fn traces_are_found_from_a_main_or_a_linked_checkout_and_nowhere_else() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("repo");
        let linked = tmp.path().join("wt");
        let common = main.join(".git");
        let wt_dir = common.join("worktrees/wt");
        std::fs::create_dir_all(&wt_dir).unwrap();
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", wt_dir.display()),
        )
        .unwrap();
        std::fs::write(wt_dir.join("commondir"), "../..\n").unwrap();
        assert!(!has_traces(&main));
        assert!(!has_traces(&linked));
        std::fs::create_dir_all(common.join("outfit")).unwrap();
        assert!(has_traces(&main));
        assert!(has_traces(&linked));
        let other = tmp.path().join("other");
        std::fs::create_dir_all(other.join(".outfit")).unwrap();
        assert!(has_traces(&other));
    }

    #[test]
    fn only_outfits_own_not_set_up_refusal_triggers_an_init() {
        let not_set_up = br#"{"error":"outfit is not installed in this repository yet (no hook dispatcher, so nothing would keep a swapped file out of a commit)","fix":"outfit init","retryable":false,"code":1}"#;
        assert!(asks_for_init(not_set_up));
        assert!(!asks_for_init(
            br#"{"error":"no profile called \"x\"","fix":"outfit ls"}"#
        ));
        assert!(!asks_for_init(
            br#"{"refused":["AGENTS.md"],"fixes":["outfit init"]}"#
        ));
        assert!(!asks_for_init(b"not json"));
    }

    #[test]
    fn a_refusal_reads_both_of_outfits_failure_shapes() {
        let error = br#"{"error":"no profile called \"nope\"","fix":"outfit ls","code":2}"#;
        assert_eq!(
            refusal(error, b""),
            "no profile called \"nope\" (fix: outfit ls)"
        );
        let human = br#"{"needs_human":true,"refused":["AGENTS.md has uncommitted changes"],"fixes":["git stash push -- AGENTS.md"]}"#;
        assert_eq!(
            refusal(human, b""),
            "AGENTS.md has uncommitted changes (fix: git stash push -- AGENTS.md)"
        );
        assert_eq!(refusal(b"not json", b"boom\n"), "boom");
    }
}
