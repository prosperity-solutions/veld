//! `veld settings` — read and write user preferences from the CLI.
//!
//! # Why this exists
//!
//! Settings stopped being about how the IDE looks. `terminal.shell` decides which
//! shell every terminal and every config-declared pane opens and which shell the
//! daemon asks for the user's `PATH`; `worktree.storageMode`/`storageDir` decide
//! where new checkouts land; `keepAwake.*` decides whether Veld may keep the
//! machine from sleeping and whether it may write a durable system power setting.
//! An agent driving Veld could start runs, share environments and read logs but
//! could not answer "is keep-awake on, and for how long" without a browser.
//!
//! # Reads open the database, writes go through the daemon
//!
//! Not a preference — the two halves have different requirements.
//!
//! A **read** has no side effects and must work when the daemon is down, which is
//! exactly when someone is most likely to be asking what a setting says. It opens
//! the database directly, the way `veld config vars` does.
//!
//! A **write** has two side effects that live in the daemon's process and cannot
//! be performed from here: re-publishing `terminal.shell` into
//! `veld_core::user_path`'s cache (a process-local cell in the *daemon*), and
//! nudging the keep-awake reconcile so a change mid-share takes effect now rather
//! than at its next tick. Writing the database behind a running daemon's back
//! stores the right bytes and silently skips both.
//!
//! So a write goes to `PATCH`/`DELETE /api/settings` when a daemon that owns this
//! database answers. When one does not, there are three distinct things that can
//! be true, and **only the first two fall back** to writing the file directly —
//! which is why `DaemonError` has separate variants rather than one:
//!
//! - **Nothing is listening**, or what is listening keeps a different database
//!   (409) — `NoDaemon`. Falls back, and is fully correct doing so: a daemon that
//!   is not running has no cache to stale and no hold to reconcile, and re-reads
//!   both at startup.
//! - **A daemon is running but predates this route** (404/405 on the `DELETE`) —
//!   `OlderDaemon`. Falls back, and the bytes land correctly, but the side effects
//!   are genuinely lost because that daemon cannot be told. So this case says
//!   *restart it*, and must not claim nothing is running.
//! - **A daemon is there and the exchange failed** — a timeout, a dropped
//!   connection, a success whose body could not be read: `Failed`. This one does
//!   **not** fall back. It fails loudly, because writing directly here would skip
//!   the side effects behind a live daemon's back and could apply a patch the
//!   daemon has already committed a second time.
//!
//! Each says which happened rather than failing silently, since "did that apply?"
//! is the one question these paths answer differently — the two fallbacks through
//! a note on stderr, `Failed` and a daemon's own `Refused` through the command's
//! error path.
//!
//! # Clamps are reported, never swallowed
//!
//! `veld_core`'s rule is that numbers clamp and enums reject. A clamp is invisible
//! to a caller that only reads the exit code, so `set` compares what it asked for
//! against the effective value it gets back and says when they differ. That is the
//! half of this command an agent most needs: without it, a wrong guess looks
//! exactly like a success.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use veld_core::db::{CatalogEntry, Choices, Db, RuntimeSource, ValueShape, catalog};

use crate::output;

/// Where an effective value came from.
const FROM_SET: &str = "set";
const FROM_DEFAULT: &str = "default";

/// `veld settings [prefix]` — every setting, or those under one key prefix.
pub async fn list(prefix: Option<String>, json: bool) -> i32 {
    let Some(db) = crate::commands::open_db(json) else {
        return 1;
    };
    let (effective, stored) = match read_all(&db) {
        Ok(v) => v,
        Err(e) => {
            output::print_error(&e, json);
            return 1;
        }
    };

    let entries = catalog();
    let matches = |key: &str| match prefix.as_deref() {
        // A bare prefix matches the segment, not the substring: `veld settings
        // terminal` must not also return `terminal.shell`'s neighbours by
        // accident, and more importantly `veld settings ui` must not match
        // nothing while `ui.` does.
        Some(p) => key == p || key.starts_with(&format!("{}.", p.trim_end_matches('.'))),
        None => true,
    };

    // Catalog order first (grouped, the order the settings dialog shows), then
    // any stored key this build does not recognise. An unknown key is a real
    // preference — written by a newer build — so it is listed rather than hidden,
    // with nothing claimed about what it means.
    let mut rows: Vec<(String, Option<&CatalogEntry>)> = entries
        .iter()
        .filter(|e| matches(&e.key))
        .map(|e| (e.key.clone(), Some(e)))
        .collect();
    let known: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
    for key in effective.keys() {
        if !known.contains(&key.as_str()) && matches(key) {
            rows.push((key.clone(), None));
        }
    }

    if rows.is_empty() {
        let msg = match prefix.as_deref() {
            Some(p) => format!("No settings under {p:?}."),
            None => "No settings.".to_string(),
        };
        if json {
            println!("{}", to_json(&json!({ "settings": [] })));
        } else {
            output::print_info(&msg);
        }
        return 0;
    }

    if json {
        let items: Vec<Value> = rows
            .iter()
            .map(|(key, entry)| {
                let mut item = json!({
                    "key": key,
                    "value": effective.get(key).cloned().unwrap_or(Value::Null),
                    "from": from_of(key, &stored),
                });
                if let Some(e) = entry {
                    item["title"] = json!(e.title);
                    item["type"] = json!(e.value_type);
                    item["group"] = json!(e.group);
                    item["default"] = e.default.clone();
                } else {
                    // Said plainly rather than left to be inferred from absent
                    // fields: this build cannot describe the key, which is a
                    // different fact from the key having no description.
                    item["known"] = json!(false);
                }
                item
            })
            .collect();
        println!("{}", to_json(&json!({ "settings": items })));
        return 0;
    }

    let table: Vec<Vec<String>> = rows
        .iter()
        .map(|(key, entry)| {
            vec![
                // The key is neutralised too: an unknown key is chosen by
                // whoever wrote it, not by this build. See `render`.
                output::one_line(key),
                render(effective.get(key).unwrap_or(&Value::Null)),
                from_of(key, &stored).to_string(),
                entry
                    .map(|e| e.title.to_string())
                    .unwrap_or_else(|| "(not known to this version of veld)".to_string()),
            ]
        })
        .collect();
    output::print_table(&["SETTING", "VALUE", "FROM", "WHAT IT IS"], &table);
    output::print_info("veld settings describe <setting> explains one, with its allowed values.");
    0
}

/// `veld settings get <key>` — one effective value, and nothing else on stdout.
///
/// Bare on purpose: `FONT=$(veld settings get terminal.fontFamily)` is the shape
/// this is for, so a scalar prints unquoted with no decoration. `--json` gives the
/// same value with its provenance.
pub async fn get(key: String, json: bool) -> i32 {
    let Some(db) = crate::commands::open_db(json) else {
        return 1;
    };
    let (effective, stored) = match read_all(&db) {
        Ok(v) => v,
        Err(e) => {
            output::print_error(&e, json);
            return 1;
        }
    };
    let Some(value) = effective.get(&key) else {
        output::print_error(&unknown_key(&key), json);
        return 1;
    };
    if json {
        println!(
            "{}",
            to_json(&json!({
                "key": key,
                "value": value,
                "from": from_of(&key, &stored),
            }))
        );
    } else {
        println!("{}", render(value));
    }
    0
}

/// `veld settings describe <key>` — what it is, what it accepts, what it defaults
/// to.
///
/// The half that makes the rest usable without guessing. Without the allowed
/// values and the range, `set` is trial and error against a daemon that clamps
/// silently, so a wrong guess reads as a success.
pub async fn describe(key: String, json: bool) -> i32 {
    let Some(entry) = catalog().into_iter().find(|e| e.key == key) else {
        // Deliberately distinct from `get`'s error: this key may well hold a
        // value (an unknown key is preserved), it just cannot be described.
        output::print_error(
            &format!(
                "{}\nveld settings get {key} still reads it if a newer veld wrote it.",
                unknown_key(&key)
            ),
            json,
        );
        return 1;
    };

    if json {
        println!("{}", to_json(&json!(entry)));
        return 0;
    }

    println!("{}", entry.key);
    println!("  {}", entry.title);
    println!();
    for line in wrap(entry.help, 76) {
        println!("  {line}");
    }
    println!();
    println!("  type      {}", entry.value_type);
    println!("  default   {}", render(&entry.default));
    println!("  group     {}", entry.group_label);
    match entry.choices {
        Choices::Static { options } => {
            println!(
                "  values    {}",
                options
                    .iter()
                    .map(|c| c.value)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            for c in options {
                println!("              {:<12} {}", c.value, c.label);
            }
        }
        Choices::Range {
            min,
            max,
            unit,
            empty_means,
            ..
        } => {
            println!(
                "  range     {min}–{max}{}",
                unit.map(|u| format!(" {u}")).unwrap_or_default()
            );
            // The clamp rule, said where somebody is about to type a number.
            println!("            out-of-range values are clamped, not refused");
            if let Some(meaning) = empty_means {
                println!("            {min} means {meaning}");
            }
        }
        Choices::Presets {
            offered,
            min,
            max,
            unit,
            ..
        } => {
            println!(
                "  range     {min}–{max}{}",
                unit.map(|u| format!(" {u}")).unwrap_or_default()
            );
            println!("            out-of-range values are clamped, not refused");
            println!(
                "  offered   {}",
                offered
                    .iter()
                    .map(|c| c.value)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            println!("            any value in range is accepted; these are what the UI shows");
        }
        Choices::Runtime { source } => {
            // Was `{source:?}`, which printed the Rust variant name ("Shells").
            // A CLI's output is not a place to leak an internal spelling.
            let what = match source {
                RuntimeSource::Shells => "the shells installed on this machine",
                RuntimeSource::Fonts => "the fonts this machine can render",
                RuntimeSource::Directory => "a folder on this machine",
            };
            println!("  values    {what}; any valid value is accepted");
        }
        // Free means "no closed set", which is not the same as "any text" — 27
        // of these are booleans, and printing "any text" directly under
        // `type bool` made the command contradict itself in the same paragraph
        // while `set` correctly refused anything but a bool. What a caller needs
        // here is what `coerce` will actually take, so it comes off the shape.
        Choices::Free => {
            let accepted = if entry.value_type == ValueShape::Bool.as_str() {
                "true or false"
            } else if entry.value_type == ValueShape::TextList.as_str() {
                "a JSON array, or a comma-separated list"
            } else if entry.value_type == ValueShape::Int.as_str() {
                "a whole number"
            } else {
                "any text the validator accepts"
            };
            println!("  values    {accepted}");
        }
    }
    if let Some(dep) = entry.requires {
        match dep.equals {
            Some(v) => println!("  requires  {} = {v}", dep.key),
            None => println!("  requires  {} = true", dep.key),
        }
    }
    0
}

/// `veld settings set <key> <value>`.
pub async fn set(key: String, raw: String, json: bool) -> i32 {
    let entry = catalog().into_iter().find(|e| e.key == key);
    let Some(entry) = entry else {
        output::print_error(&unknown_key(&key), json);
        return 1;
    };

    let value = match coerce(&raw, &entry) {
        Ok(v) => v,
        Err(e) => {
            output::print_error(&e, json);
            return 1;
        }
    };

    let mut patch = BTreeMap::new();
    patch.insert(key.clone(), value.clone());

    let effective = match write(Write::Patch(patch), json).await {
        Ok(written) => written.effective,
        Err(e) => {
            output::print_error(&e, json);
            return 1;
        }
    };

    let stored = effective.get(&key).cloned().unwrap_or(Value::Null);
    // The clamp, made visible. `veld_core` rounds a float and clamps a number to
    // its range without complaining, which is right for a slider and wrong for
    // somebody who just typed 600 and would otherwise believe it took.
    let clamped = stored != value;

    if json {
        println!(
            "{}",
            to_json(&json!({
                "key": key,
                "requested": value,
                "value": stored,
                "clamped": clamped,
            }))
        );
        return 0;
    }
    output::print_success(&format!("{key} = {}", render(&stored)));
    if clamped {
        output::print_info(&format!(
            "{} was adjusted to {} — see veld settings describe {key}",
            render(&value),
            render(&stored)
        ));
    }
    0
}

/// `veld settings unset <key>` — back to the default.
pub async fn unset(key: String, json: bool) -> i32 {
    // Unknown keys are unsettable on purpose: a preference a newer veld wrote is
    // still this user's, and refusing to clear it would leave them with no way to
    // undo it from a build that had rolled back.
    let written = match write(Write::Delete(key.clone()), json).await {
        Ok(written) => written,
        Err(e) => {
            output::print_error(&e, json);
            return 1;
        }
    };
    let now = written.effective.get(&key).cloned().unwrap_or(Value::Null);
    // `Some(0)` is the honest "there was nothing to clear" — the answer for a
    // mistyped key, which otherwise gets a confident success and exit 0. `None`
    // is a daemon too old to say, and must not be reported as either.
    let was_set = written.removed.map(|n| n > 0);

    if json {
        let mut body = json!({ "key": key, "value": now });
        if let Some(was_set) = was_set {
            body["removed"] = json!(was_set);
        }
        println!("{}", to_json(&body));
        return 0;
    }
    if was_set == Some(false) {
        // Not an error: unsetting something already unset is a no-op the caller
        // asked for. But it must not read as though a stored value was cleared.
        output::print_info(&match catalog().into_iter().find(|e| e.key == key) {
            Some(_) => format!("{key} was already on its default, {}", render(&now)),
            None => format!("{key} was not set."),
        });
        return 0;
    }
    match catalog().into_iter().find(|e| e.key == key) {
        Some(_) => {
            output::print_success(&format!("{key} is back on its default, {}", render(&now)))
        }
        // A key this build cannot describe has no default to return to — the row
        // is simply gone, and saying "back to its default" would be a claim about
        // a value this binary does not have.
        None => output::print_success(&format!("{key} cleared")),
    }
    0
}

// ---------------------------------------------------------------------------
// Plumbing
// ---------------------------------------------------------------------------

enum Write {
    Patch(BTreeMap<String, Value>),
    Delete(String),
}

/// What a write produced: the effective document, and — for an unset — how many
/// stored rows it actually removed.
struct Written {
    effective: BTreeMap<String, Value>,
    /// `None` for a patch. `Some(0)` means the key was already on its default,
    /// which is the difference between reporting a reset and fabricating one.
    removed: Option<usize>,
}

/// Apply a write through the daemon, or directly if it is not running.
///
/// Returns the effective settings document afterwards, which is what makes a
/// clamp visible — the daemon echoes it, and the direct path re-reads it.
async fn write(what: Write, json: bool) -> Result<Written, String> {
    match send_to_daemon(&what).await {
        Ok(doc) => Ok(doc),
        Err(DaemonError::Refused(message)) | Err(DaemonError::Failed(message)) => Err(message),
        Err(kind @ (DaemonError::NoDaemon | DaemonError::OlderDaemon)) => {
            // Not a warning: this is a correct path, and the note exists so the
            // difference is on the record rather than because anything is wrong.
            // A daemon that is not running has no shell cache to stale and no
            // keep-awake hold to reconcile, and re-reads both when it starts.
            // `eprintln!` rather than `output::print_info`, which is a `println!`
            // — this note must not land on stdout, where `--json` output and
            // `veld settings get`'s bare value live. The `!json` guard is a
            // second belt, not the reason.
            if !json {
                eprintln!(
                    "{}",
                    match kind {
                        DaemonError::OlderDaemon =>
                            "Your veld daemon is older than this veld and cannot apply this \
                             change itself; writing it directly. Restart the daemon (or finish \
                             `veld update`) for it to take full effect.",
                        _ =>
                            "No running veld daemon owns this settings database; writing it \
                             directly.",
                    }
                );
            }
            let db = veld_core::db::Db::open()
                .map_err(|e| format!("Failed to open veld database: {e}"))?;
            let removed = match &what {
                Write::Patch(patch) => {
                    db.patch_settings(patch)
                        .map_err(|e| format!("veld refused that value: {e}"))?;
                    None
                }
                Write::Delete(key) => Some(
                    db.unset_settings(std::slice::from_ref(key))
                        .map_err(|e| format!("Failed to clear the setting: {e}"))?,
                ),
            };
            read_all(&db).map(|(effective, _)| Written { effective, removed })
        }
    }
}

enum DaemonError {
    /// No daemon **of this database** is available: nothing is listening, or what
    /// answered keeps a different settings file (409). Both are legitimate and
    /// both have the same right answer — write the file this process reads.
    ///
    /// A daemon that is merely *too old* for the route is [`Self::OlderDaemon`],
    /// not this: it is running and does own the database, so it needs a different
    /// sentence.
    NoDaemon,
    /// A daemon answered and said no. Its sentence, which is the validator's.
    Refused(String),
    /// A daemon answered but predates this route. The write still has to happen
    /// — it just cannot be delivered through the daemon, so its caches stay stale
    /// until it restarts, and the user is told that rather than told nothing is
    /// running.
    OlderDaemon,
    /// A daemon is **there** and the exchange did not complete: a timeout, a
    /// connection dropped mid-flight, a success whose body could not be read.
    ///
    /// Deliberately *not* `NoDaemon`. Falling back on these would write the
    /// database behind a live daemon's back — silently skipping the shell-cache
    /// re-publish and the keep-awake nudge that are the entire reason a write
    /// goes through the daemon — while printing "no running veld daemon", which
    /// is false. On a timeout it can also apply the write twice, since the
    /// daemon may well have committed the patch already.
    Failed(String),
}

/// Whether the daemon this process can reach is the one that owns the database
/// this process reads.
///
/// **Decided here, before the request, and not only by the daemon's own check.**
/// The server-side `require_same_db` guard in the daemon cannot cover the case
/// that matters most: an *older* daemon does not know the header, ignores it, and
/// writes its own database anyway. A new CLI is exactly the thing likely to meet
/// an old daemon.
///
/// Two ways the pairing is sound:
///
/// - this process reads the installed user database, so the daemon on the default
///   port is reading it too; or
/// - a port was named explicitly, which by this repo's dev-stack convention means
///   the caller was handed a matching `VELD_DB_PATH` (`just dev`, and every node
///   of `veld start --preset dev`).
///
/// Everything else — most importantly a bare `cargo run`, which resolves to
/// `.veld-dev/veld-cargo.db` while the default port answers from the *real* one —
/// writes directly instead. That combination silently changed a developer's real
/// settings during this feature's own smoke test.
fn daemon_owns_our_database() -> bool {
    veld_core::db::Db::uses_installed_database() || veld_core::instance::daemon_port_is_explicit()
}

async fn send_to_daemon(what: &Write) -> Result<Written, DaemonError> {
    if !daemon_owns_our_database() {
        return Err(DaemonError::NoDaemon);
    }
    let base = veld_core::instance::daemon_base();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| DaemonError::Failed(format!("could not build an HTTP client: {e}")))?;
    let request = match what {
        Write::Patch(patch) => client.patch(format!("{base}/api/settings")).json(patch),
        // Percent-encoded because a key is a path segment and this one is user
        // input — `veld settings unset ../../something` must reach the handler as
        // a key, not as a path.
        Write::Delete(key) => client.delete(delete_url(&base, key)),
    };
    // Which database *this* process reads. The daemon compares it against the one
    // it opened and answers 409 if they differ — see `require_same_db` there. It
    // is sent rather than assumed because the two can genuinely diverge: a
    // cargo-built `veld` resolves to `.veld-dev/veld-cargo.db` by design, while
    // the daemon on the default port is the installed one on the real database,
    // so without this a `set` writes a file the matching `get` never reads.
    // Percent-encoded, because a `HeaderValue` builder accepts bytes a
    // `HeaderValue::to_str` cannot read back — so a database path with one
    // non-ASCII byte reached the daemon as a header it decoded as *absent* and
    // then allowed. The daemon encodes its own path the same way and compares
    // the encoded forms; `veld_core::percent` is deliberately the only encoder.
    let our_db = veld_core::db::Db::default_path()
        .map(|p| veld_core::percent::encode_component(&p.to_string_lossy()))
        .unwrap_or_default();

    let resp = request
        // The header `check_csrf` wants. A CLI is not a browser, but the daemon
        // cannot tell the difference and should not have to.
        .header("X-Veld-Request", "1")
        .header("X-Veld-Db", our_db)
        .send()
        .await
        // Only a *connect* failure means there is no daemon. A timeout or a
        // dropped connection means one is there and something went wrong, which
        // must be reported rather than quietly turned into a direct write.
        .map_err(|e| {
            if e.is_connect() {
                DaemonError::NoDaemon
            } else {
                DaemonError::Failed(format!("the veld daemon did not answer: {e}"))
            }
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        // A daemon that keeps a different database is not *this* CLI's daemon, so
        // it is no more relevant than one that is not running — take the direct
        // path, which writes the file this process would read.
        //
        // 404/405 mean the same thing for a different reason: `DELETE
        // /api/settings/{key}` is new in this change, so a daemon from before it
        // has no such route. `PATCH` has always existed, so only the unset can
        // land here — and refusing would leave `veld settings unset` broken for
        // anyone whose daemon has not been restarted since the update, which is
        // everybody for the first minutes after one.
        if status == reqwest::StatusCode::CONFLICT {
            return Err(DaemonError::NoDaemon);
        }
        // Older daemon: the route does not exist there. Still a fallback, but a
        // *different* one — that daemon is running and does own this database, so
        // the write lands correctly while its in-process caches do not learn
        // about it. Saying "no running veld daemon" here would be false in the
        // one case where the daemon is alive to be told, and the sentence the
        // fallback prints is what a user reads to decide whether the change
        // applied.
        if matches!(
            status,
            reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::METHOD_NOT_ALLOWED
        ) {
            return Err(DaemonError::OlderDaemon);
        }
        let body = resp.text().await.unwrap_or_default();
        // The daemon's `ApiError` puts the validator's sentence under `error`.
        let message = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_owned))
            .unwrap_or_else(|| format!("the daemon refused the write ({status})"));
        return Err(DaemonError::Refused(message));
    }

    #[derive(serde::Deserialize)]
    struct SettingsResponse {
        settings: BTreeMap<String, Value>,
        /// Absent on a patch, and absent from a daemon older than this change —
        /// which is why it is an `Option` rather than a defaulted `0`: "the
        /// daemon did not say" and "nothing was removed" are different facts, and
        /// only the second one may be reported as such.
        #[serde(default)]
        removed: Option<usize>,
    }
    // The status already said the write succeeded, so this is not a fallback
    // point: the daemon has applied it, and writing directly here would apply it
    // a second time and report that no daemon was running.
    let parsed: SettingsResponse = resp.json().await.map_err(|e| {
        DaemonError::Failed(format!(
            "the veld daemon applied the change but its reply could not be read: {e}"
        ))
    })?;
    Ok(Written {
        effective: parsed.settings,
        removed: parsed.removed,
    })
}

/// Where a `DELETE` for one setting goes.
///
/// Its own function so a test can pin it. The key is user input and a path
/// segment, so it is percent-encoded — `veld settings unset ../../something` has
/// to reach the handler as a key, not as a path. Inlining this made the test that
/// claimed to check it pass while only re-testing the encoder.
fn delete_url(base: &str, key: &str) -> String {
    format!(
        "{base}/api/settings/{}",
        veld_core::percent::encode_component(key)
    )
}

/// The effective document plus which keys have a stored row.
fn read_all(db: &Db) -> Result<(BTreeMap<String, Value>, Vec<String>), String> {
    let effective = db
        .settings()
        .map_err(|e| format!("Failed to read settings: {e}"))?;
    let stored = db
        .settings_with_stored_value()
        .map_err(|e| format!("Failed to read settings: {e}"))?;
    Ok((effective, stored))
}

fn from_of(key: &str, stored: &[String]) -> &'static str {
    if stored.iter().any(|k| k == key) {
        FROM_SET
    } else {
        FROM_DEFAULT
    }
}

/// Turn what somebody typed into the JSON the setting takes.
///
/// The point of doing this from the catalog's `type` rather than guessing: a
/// bool wants `true`, not `"true"`, and a caller should not have to know that
/// `veld settings set terminal.cursorBlink true` is a JSON document. Scalars are
/// therefore written bare, and only a list needs JSON syntax.
fn coerce(raw: &str, entry: &CatalogEntry) -> Result<Value, String> {
    let shape = entry.value_type;
    if shape == ValueShape::Bool.as_str() {
        return match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Ok(Value::Bool(true)),
            "false" | "no" | "off" | "0" => Ok(Value::Bool(false)),
            other => Err(format!("{} takes true or false, not {other:?}", entry.key)),
        };
    }
    if shape == ValueShape::Int.as_str() {
        return raw
            .trim()
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| format!("{} takes a whole number, not {raw:?}", entry.key));
    }
    if shape == ValueShape::TextList.as_str() {
        // A JSON array, or a comma-separated list for the common case. Both, not
        // one: an agent already holds a JSON array, and a human typing two
        // origins should not have to quote and bracket them.
        if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(raw) {
            return Ok(Value::Array(items));
        }
        let items: Vec<Value> = raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(Value::from)
            .collect();
        return Ok(Value::Array(items));
    }
    // Text, including the enums: the value is what was typed. A closed set is
    // checked by the validator, whose refusal names the allowed values.
    Ok(Value::from(raw))
}

/// A scalar prints bare; anything else prints as JSON.
///
/// `veld settings get terminal.fontFamily` in a shell substitution must not come
/// back wrapped in quotes it would then carry into a config file.
///
/// Neutralised through [`output::one_line`], which is not belt-and-braces: this
/// change gives a stored setting value a **new sink**, a terminal, and one class
/// of value is not character-bounded on the way in. An unknown key (a preference
/// written by a newer veld) is stored verbatim with only a length limit, and the
/// daemon is reachable same-origin from a developer's own app through the
/// helper's `/__veld__` proxy — so a script on that origin can `PATCH` a key or
/// value containing `ESC`, and `veld settings` would then replay it into the
/// terminal of whoever ran it, or of an agent. `terminal.fontFamily`'s validator
/// already bounds characters for exactly this reason, one interpolation site
/// earlier; this is the same rule for the fourth site.
fn render(value: &Value) -> String {
    output::one_line(&match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    })
}

fn unknown_key(key: &str) -> String {
    format!("veld has no setting called {key:?}. Run veld settings to see them all.")
}

fn to_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string())
}

/// Wrap help text for a terminal.
///
/// The help strings are paragraphs, not labels — several are the only place a
/// behaviour is written down — so `describe` has to lay one out rather than
/// print a 900-character line.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str) -> CatalogEntry {
        catalog().into_iter().find(|e| e.key == key).unwrap()
    }

    #[test]
    fn a_bool_setting_takes_a_bare_word_not_json() {
        let e = entry("terminal.cursorBlink");
        assert_eq!(coerce("true", &e).unwrap(), Value::Bool(true));
        assert_eq!(coerce("off", &e).unwrap(), Value::Bool(false));
        assert_eq!(coerce("0", &e).unwrap(), Value::Bool(false));
        assert!(coerce("maybe", &e).is_err());
    }

    #[test]
    fn a_number_setting_refuses_a_word_rather_than_sending_a_string() {
        let e = entry("keepAwake.sharingOnBatteryMinutes");
        assert_eq!(coerce("60", &e).unwrap(), Value::from(60));
        assert!(coerce("an hour", &e).is_err());
    }

    /// Both spellings, because the two callers differ: an agent has an array
    /// already, a person typing two origins should not have to bracket them.
    #[test]
    fn a_list_setting_takes_json_or_commas() {
        let e = entry("browser.externalOrigins");
        let expected = Value::Array(vec![
            Value::from("https://a.example"),
            Value::from("https://b.example"),
        ]);
        assert_eq!(
            coerce(r#"["https://a.example","https://b.example"]"#, &e).unwrap(),
            expected
        );
        assert_eq!(
            coerce("https://a.example, https://b.example", &e).unwrap(),
            expected
        );
        assert_eq!(coerce("", &e).unwrap(), Value::Array(vec![]));
    }

    /// A text setting is stored verbatim — including something that *looks* like
    /// JSON, which a font list with quotes in it does.
    #[test]
    fn a_text_setting_is_not_parsed_as_json() {
        let e = entry("terminal.fontFamily");
        assert_eq!(
            coerce("\"JetBrains Mono\", monospace", &e).unwrap(),
            Value::from("\"JetBrains Mono\", monospace")
        );
    }

    /// A prefix filter matches whole key segments. `ui` must find `ui.*`, and
    /// `terminal` must not also drag in a key that merely starts with those
    /// letters.
    #[test]
    fn a_prefix_matches_segments_not_substrings() {
        let matches = |prefix: &str, key: &str| {
            key == prefix || key.starts_with(&format!("{}.", prefix.trim_end_matches('.')))
        };
        assert!(matches("ui", "ui.showProjectColumn"));
        assert!(matches("ui.", "ui.showProjectColumn"));
        assert!(matches("keepAwake", "keepAwake.sharingOnPower"));
        assert!(!matches("term", "terminal.shell"));
        assert!(!matches("browser.quick", "browser.quickSwitch.responsive"));
    }

    /// A scalar comes back bare so it can be used in a shell substitution
    /// without carrying quotes into whatever consumes it.
    #[test]
    fn a_string_renders_without_json_quotes() {
        assert_eq!(render(&Value::from("auto")), "auto");
        assert_eq!(render(&Value::from(12)), "12");
        assert_eq!(render(&Value::Bool(true)), "true");
        assert_eq!(render(&Value::Array(vec![Value::from("a")])), "[\"a\"]");
    }

    /// The key reaches the daemon as one path segment, whatever it contains.
    ///
    /// Asserts on the **URL this command builds**, not on the encoder — the
    /// encoder has its own tests in `veld_core::percent`, and an earlier version
    /// of this test only called it again, so deleting the encode at the call site
    /// left it green. What can regress here is the routing.
    #[test]
    fn a_key_is_percent_encoded_into_the_delete_path() {
        let base = "http://127.0.0.1:19899";
        assert_eq!(
            delete_url(base, "terminal.shell"),
            "http://127.0.0.1:19899/api/settings/terminal.shell"
        );
        // The one that matters: a traversal attempt stays one segment.
        assert_eq!(
            delete_url(base, "../../etc/passwd"),
            "http://127.0.0.1:19899/api/settings/..%2F..%2Fetc%2Fpasswd"
        );
        assert_eq!(
            delete_url(base, "a b"),
            "http://127.0.0.1:19899/api/settings/a%20b"
        );
    }

    /// Every catalog help string survives being laid out — the wrap must not
    /// drop or duplicate a word, and several of these are the only written
    /// account of a behaviour.
    #[test]
    fn wrapping_help_preserves_every_word() {
        for e in catalog() {
            let wrapped = wrap(e.help, 76);
            assert_eq!(
                wrapped.join(" ").split_whitespace().collect::<Vec<_>>(),
                e.help.split_whitespace().collect::<Vec<_>>(),
                "{} lost words in the wrap",
                e.key
            );
            assert!(
                wrapped
                    .iter()
                    .all(|l| l.chars().count() <= 76 || l.split_whitespace().count() == 1),
                "{} produced an over-long line",
                e.key
            );
        }
    }
}
