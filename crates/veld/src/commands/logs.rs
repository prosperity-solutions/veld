use veld_core::config;
use veld_core::db::{LogRow, LogTimeZone};
use veld_core::logging;
use veld_engine::api::{LogLine, LogsPage};

use super::status::client_failed;
use crate::engine_client::EngineClient;
use crate::output;

/// Log source filter for `--source` flag. Lives in the engine, which maps it to
/// the stored streams.
pub use veld_engine::api::SourceFilter;

pub struct LogsOptions {
    pub name: Option<String>,
    pub node: Option<String>,
    pub lines: usize,
    pub since: Option<String>,
    pub follow: bool,
    pub json: bool,
    pub source: SourceFilter,
    pub search: Option<String>,
    pub context_lines: usize,
    /// Address a specific past run by id prefix.
    pub run: Option<String>,
    /// The run before the latest one.
    pub previous: bool,
    /// Every run under the name interleaved (pre-v3 behavior, includes
    /// legacy unscoped rows).
    pub all_runs: bool,
    /// Which zone to print timestamps in, or `None` to follow the
    /// `logs.timeZone` setting. `--utc`/`--local` set it for one invocation.
    pub time_zone: Option<LogTimeZone>,
}

fn source_name(s: SourceFilter) -> &'static str {
    match s {
        SourceFilter::All => "all",
        SourceFilter::Server => "server",
        SourceFilter::Setup => "setup",
        SourceFilter::Internal => "internal",
    }
}

/// `veld orchestrator logs [--name <n>] [--node <n>] [--lines <n>] [--since <d>] [-f] [--json] [--source <s>] [--search <term>] [--context <n>]`
///
/// The engine (in the daemon) resolves the run, reads and filters the rows;
/// this prints them and, with `-f`, polls it for newer ones.
pub async fn run(opts: LogsOptions) -> i32 {
    let LogsOptions {
        name,
        node,
        lines,
        since,
        follow,
        json,
        source,
        search,
        context_lines,
        run,
        previous,
        all_runs,
        time_zone,
    } = opts;
    let Some((config_path, _cfg)) = super::parse_config(json) else {
        return 1;
    };
    let project_root = config::project_root(&config_path);

    let client = match EngineClient::connect().await {
        Ok(c) => c,
        Err(e) => return client_failed(&e, json),
    };

    let mut query: Vec<(&str, String)> = vec![
        ("project_root", project_root.to_string_lossy().into_owned()),
        ("lines", lines.to_string()),
        ("source", source_name(source).to_owned()),
        ("context", context_lines.to_string()),
        ("previous", previous.to_string()),
        ("all_runs", all_runs.to_string()),
    ];
    for (key, value) in [
        ("name", &name),
        ("node", &node),
        ("since", &since),
        ("search", &search),
        ("run", &run),
    ] {
        if let Some(v) = value {
            query.push((key, v.clone()));
        }
    }
    let page: LogsPage = match client.get("/api/engine/logs", &query).await {
        Ok(p) => p,
        Err(e) => return client_failed(&e, json),
    };
    if let (Some(note), false) = (&page.note, json) {
        output::print_info(note);
    }
    // Flag beats setting beats default (local). Resolved once, here, so the snapshot
    // and follow mode cannot render the same run in two different zones.
    let time_zone = time_zone
        .or_else(|| LogTimeZone::parse(&page.time_zone))
        .unwrap_or_default();
    let run_name = page.run.as_str();

    // Ids the snapshot already showed past the follow watermark — follow skips
    // exactly these.
    let follow_from = page.last_id;
    let already_shown: std::collections::HashSet<i64> = page
        .rows
        .iter()
        .map(|r| r.id)
        .filter(|id| *id > follow_from)
        .collect();

    let mut all_output: Vec<serde_json::Value> = Vec::new();
    for row in &page.rows {
        // Print separator when there's a gap between visible lines.
        if row.gap_before && !json {
            println!("{}", output::dim("..."));
        }
        if json {
            let j = logging::row_to_json(&to_row(row), run_name);
            if follow {
                println!("{}", serde_json::to_string(&j).unwrap());
            } else {
                all_output.push(j);
            }
        } else {
            let text = format_row(&to_row(row), time_zone);
            if row.is_match {
                println!("{text}");
            } else {
                println!("{}", output::dim(&text));
            }
        }
    }

    if json && !follow {
        println!("{}", serde_json::to_string_pretty(&all_output).unwrap());
    }

    // Follow mode: poll for new rows continuously. Following an ended run
    // (already ended, or ending mid-follow) prints history and exits 0 —
    // polling forever on a run that can't produce lines would hang an agent
    // waiting on a crashed environment. The note goes to stderr so stdout
    // stays pure log payload. `--all-runs` has no single run to watch and
    // keeps the old poll-forever behavior.
    if follow {
        if !all_runs && !page.live {
            eprintln!(
                "run {} has ended ({}) — nothing to follow",
                page.short_id, page.outcome
            );
            return 0;
        }
        // Pinned to the resolved run so a restart mid-follow cannot move it.
        let mut poll: Vec<(&str, String)> = vec![
            ("project_root", project_root.to_string_lossy().into_owned()),
            ("name", page.run.clone()),
            ("source", source_name(source).to_owned()),
            ("all_runs", all_runs.to_string()),
        ];
        if !all_runs {
            poll.push(("run", page.run_id.clone()));
        }
        for (key, value) in [("node", &node), ("search", &search)] {
            if let Some(v) = value {
                poll.push((key, v.clone()));
            }
        }
        follow_logs(
            &client,
            poll,
            !all_runs,
            follow_from,
            already_shown,
            json,
            time_zone,
        )
        .await;
    }

    0
}

/// The row as the database stored it, for the shared renderers.
fn to_row(line: &LogLine) -> LogRow {
    LogRow {
        id: line.id,
        node: line.node.clone(),
        variant: line.variant.clone(),
        stream: line.stream.clone(),
        ts: line.ts.clone(),
        line: line.line.clone(),
    }
}

/// Human-readable label + line for one log row.
///
/// `tz` decides only how the stored UTC timestamp is *spelled* — see
/// `logging::format_ts`. The `--json` path never comes through here.
fn format_row(row: &LogRow, tz: LogTimeZone) -> String {
    let label = match (&row.node, row.stream.as_str()) {
        (Some(node), _) => {
            output::cyan(&format!("{node}:{}", row.variant.as_deref().unwrap_or("?")))
        }
        (None, stream) => output::cyan(&format!("_veld:{stream}")),
    };
    format!("{label} [{}] {}", logging::format_ts(&row.ts, tz), row.line)
}

/// Poll the engine for new rows, printing them as they appear, until Ctrl+C —
/// or until the followed run reaches a terminal status (one final drain, then
/// exit; a run that ended produces no further lines and an agent must not block
/// on it forever). `watch_run` is false under `--all-runs`, which has no single
/// run to watch. `already_shown` holds ids past the watermark that the
/// historical snapshot already printed.
async fn follow_logs(
    client: &EngineClient,
    poll: Vec<(&str, String)>,
    watch_run: bool,
    mut last_id: i64,
    mut already_shown: std::collections::HashSet<i64>,
    json: bool,
    tz: LogTimeZone,
) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));
    // Check the run's status only every ~2s (every 10th tick), as before.
    let mut tick: u64 = 0;
    let mut final_drain = false;
    let mut drain_ticks: u64 = 0;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let mut query = poll.clone();
                query.push(("after_id", last_id.to_string()));
                // A transient failure (the daemon restarting under an update)
                // skips the tick, as a transient DB error used to.
                let Ok(page) = client.get::<LogsPage>("/api/engine/logs", &query).await else {
                    continue;
                };
                let quiet_tick = page.last_id == last_id;
                last_id = last_id.max(page.last_id);
                for row in &page.rows {
                    if already_shown.remove(&row.id) {
                        continue; // the snapshot already printed this line
                    }
                    if json {
                        let entry = logging::row_to_json(&to_row(row), &page.run);
                        println!("{}", serde_json::to_string(&entry).unwrap());
                    } else {
                        println!("{}", format_row(&to_row(row), tz));
                    }
                }

                if final_drain {
                    // Drain until a quiet tick (detached `--log-sink` writers can
                    // trail the finalize by more than one 200ms tick — the
                    // trailing lines are often the crash output itself),
                    // bounded so a still-chattering wrapper can't hold the
                    // exit forever.
                    drain_ticks += 1;
                    if quiet_tick || drain_ticks >= 25 {
                        return;
                    }
                    continue;
                }
                tick += 1;
                if tick % 10 == 0 && watch_run && !page.live {
                    eprintln!("run has ended — stopping follow");
                    final_drain = true;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use veld_core::db::LogStream;

    fn row(ts: &str) -> LogRow {
        LogRow {
            id: 1,
            node: Some("web".to_owned()),
            variant: Some("local".to_owned()),
            stream: LogStream::Server.as_str().to_owned(),
            ts: ts.to_owned(),
            line: "listening on 3000".to_owned(),
        }
    }

    #[test]
    fn utc_output_is_unchanged_from_before_the_flag_existed() {
        // `--utc` is the compatibility escape hatch, so it prints the stored string
        // verbatim rather than re-deriving one that merely means the same instant.
        let text = format_row(&row("2026-08-06T09:12:33.123456Z"), LogTimeZone::Utc);
        assert!(text.contains("[2026-08-06T09:12:33.123456Z]"), "{text}");
        assert!(text.ends_with("listening on 3000"), "{text}");
    }

    #[test]
    fn local_output_stays_bracketed_and_parseable() {
        // The bracket is what every downstream reader of this output keys on — a
        // person scanning, and the `/ide` view's own `extractTs` regex on the
        // daemon-prefixed copy of the same shape. Localising must not change it.
        let text = format_row(&row("2026-08-06T09:12:33.123456Z"), LogTimeZone::Local);
        let inner = text
            .split_once('[')
            .and_then(|(_, rest)| rest.split_once(']'))
            .map(|(ts, _)| ts)
            .expect("still one bracketed timestamp");
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(inner)
                .expect("still RFC 3339")
                .to_utc(),
            chrono::DateTime::parse_from_rfc3339("2026-08-06T09:12:33.123456Z")
                .unwrap()
                .to_utc()
        );
    }
}
