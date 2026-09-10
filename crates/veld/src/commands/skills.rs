//! `veld skills` — the agent-facing documentation, carried by the binary it
//! documents.
//!
//! # Why the binary and not a file
//!
//! Veld ships consumer skills in the repo's `skills/` directory for the
//! `npx skills` ecosystem. Those are **shells**: frontmatter, a paragraph, and a
//! pointer here. Everything an agent actually reads lives in `src/commands/skills/`
//! and is `include_str!`d into the binary, for three reasons that are not
//! interchangeable:
//!
//! * **A shell never goes stale.** A skill file installed into somebody's project
//!   is a copy, frozen at the moment they ran `npx skills add`. Documentation for
//!   `veld` that lives in that copy describes whichever veld they had that day.
//!   Documentation that lives in the binary describes *the binary they are running*,
//!   which is the only version whose behaviour matters to the answer.
//! * **A shell costs nothing to load.** A skill's whole body enters an agent's
//!   context the moment the skill matches, whether or not the task needs it — and
//!   the previous `skills/veld/SKILL.md` was 100KB of that, plus a further ~35KB
//!   from six `!`-prefixed shell invocations it ran at load time (this repo's own
//!   `veld config` is 35KB of JSONC on its own). An index the agent can read in
//!   twenty lines, and one topic it then asks for, is the same information at a
//!   fraction of the cost — and the state half (`veld presets`, `veld nodes`) is
//!   better fetched when the question is actually asked, since by then it is also
//!   *current*.
//! * **Drift becomes a compile error rather than a silent lie.** A topic naming a
//!   flag is next to the clap definition of that flag, in the same crate, in the
//!   same pull request. `tests/validate-doc-commands.py` reads these documents with
//!   the real binary's `--help` and fails on a command that does not exist.
//!
//! # The contract
//!
//! `veld skills` prints the index; `veld skills <topic>` prints one document. Both
//! go to **stdout** (AGENTS.md: machine-readable output on stdout) because the
//! caller is an agent piping it into its own context, and both work with no
//! `veld.json`, no daemon, and no project — an agent asking how to write a config
//! does not have one yet.

use std::io::Write;

use crate::output;

/// Write one finished document to stdout, treating a closed pipe as a normal end.
///
/// **Rust ignores SIGPIPE, so a `println!` into a reader that has already left
/// panics** — `failed printing to stdout: Broken pipe (os error 32)`, plus a
/// backtrace, on stderr. That is reachable here and effectively nowhere else in
/// this binary: `veld skills config` is 78KB against a 64KB pipe buffer, so
/// `| head -3` races the reader's exit. Measured on this tree it panicked **10
/// times in 20**, while `veld config` (35KB, a single `println!`) panicked 0 in
/// 20. Sampling a long document with `head` or `sed -n` is exactly what the
/// caller — an agent piping this into its own context — is expected to do.
///
/// One `write_all` of an already-finished string, rather than a `println!` per
/// row, so there is a single place for the error to arrive. `BrokenPipe` means
/// the reader got what it asked for: exit 0, say nothing. Every other write
/// error is still reported — a full disk behind a redirect is not success.
fn emit(text: &str) -> i32 {
    let mut out = std::io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => 0,
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => 0,
        Err(e) => {
            output::print_error(&format!("could not write to stdout: {e}"), false);
            1
        }
    }
}

/// `text` with exactly one trailing newline, the way `println!` would leave it.
fn line(text: &str) -> String {
    if text.ends_with('\n') {
        text.to_owned()
    } else {
        format!("{text}\n")
    }
}

/// One document: the name an agent asks for, the one-line summary the index
/// shows, and the text itself.
///
/// `summary` lives here rather than being parsed out of the document, so the
/// index costs no markdown parsing and a document is free to open however it
/// likes. It is what an agent reads to decide whether to spend the topic.
struct Topic {
    name: &'static str,
    summary: &'static str,
    body: &'static str,
}

/// Declared in reading order, not alphabetically: the index doubles as a
/// suggested path through the material, and `basics` is first because it is the
/// one an agent should read before it types anything.
const TOPICS: &[Topic] = &[
    Topic {
        name: "basics",
        summary: "Start here. What veld is, the command surface, and the rules that stop an \
                  agent guessing wrong.",
        body: include_str!("skills/basics.md"),
    },
    Topic {
        name: "runs",
        summary: "Environments vs runs, run history and post-mortems, and one-off runs \
                  (`--oneshot`) for tests and CI.",
        body: include_str!("skills/runs.md"),
    },
    Topic {
        name: "outputs",
        summary: "Reading a run: node outputs, logs, and resource usage (`veld stats`).",
        body: include_str!("skills/outputs.md"),
    },
    Topic {
        name: "actions",
        summary: "Node-defined actions — commands a node exposes to the CLI and the IDE, with \
                  its live outputs injected.",
        body: include_str!("skills/actions.md"),
    },
    Topic {
        name: "config",
        summary: "Authoring veld.json: schema, nodes, variants, presets, probes, ports, vars, \
                  secrets, interpolation.",
        body: include_str!("skills/config.md"),
    },
    Topic {
        name: "gotchas",
        summary: "The mistakes that look right. Read before authoring or debugging a config.",
        body: include_str!("skills/gotchas.md"),
    },
    Topic {
        name: "sharing",
        summary: "Share a running environment peer-to-peer, or publish it on the web gateway.",
        body: include_str!("skills/sharing.md"),
    },
    Topic {
        name: "feedback",
        summary: "Get a human to look at your work in the browser, and drain their comments \
                  one at a time.",
        body: include_str!("skills/feedback.md"),
    },
    Topic {
        name: "ide-extensions",
        summary: "Customize the IDE top bar for a project: status badges, buttons, menus \
                  (`ide.extensions`).",
        body: include_str!("skills/ide-extensions.md"),
    },
    Topic {
        name: "ide-panes",
        summary: "Declare terminal and browser panes, run a coding agent in one, and offer its \
                  earlier sessions (`ide.panes`).",
        body: include_str!("skills/ide-panes.md"),
    },
    Topic {
        name: "ide-news",
        summary: "Tell this project's team something changed (`ide.news`).",
        body: include_str!("skills/ide-news.md"),
    },
    Topic {
        name: "install",
        summary: "Installing, updating and uninstalling veld, and what each mode needs.",
        body: include_str!("skills/install.md"),
    },
    Topic {
        name: "troubleshooting",
        summary: "Ports, health checks, DNS, certificates, the daemon — what to check and in \
                  what order.",
        body: include_str!("skills/troubleshooting.md"),
    },
];

fn find(name: &str) -> Option<&'static Topic> {
    TOPICS.iter().find(|t| t.name == name)
}

/// `veld skills --json`. `bytes` is the document's size: the caller has a context
/// budget, the topics run from 4K to 78K, and this is what lets it choose before
/// it spends a call.
fn index_json() -> serde_json::Value {
    serde_json::Value::Array(
        TOPICS
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "summary": t.summary,
                    "bytes": t.body.len(),
                })
            })
            .collect(),
    )
}

/// `veld skills <topic> --json`.
fn topic_json(t: &Topic) -> serde_json::Value {
    serde_json::json!({ "name": t.name, "summary": t.summary, "body": t.body })
}

/// `veld skills [TOPIC] [--json]`
///
/// Deliberately synchronous and dependency-free: no config, no database, no
/// daemon. The whole answer is a compile-time string, which is also why this is
/// one of the commands an in-progress `veld update` does not block — an agent
/// reading its own instructions must never be told to come back later.
pub fn run(topic: Option<String>, json: bool) -> i32 {
    match topic {
        None => {
            if json {
                emit(&line(
                    &serde_json::to_string_pretty(&index_json()).unwrap_or_default(),
                ))
            } else {
                emit(&render_index())
            }
        }
        Some(name) => match find(&name) {
            Some(t) => {
                if json {
                    emit(&line(
                        &serde_json::to_string_pretty(&topic_json(t)).unwrap_or_default(),
                    ))
                } else {
                    // Emitted raw. The body is compile-time text from this repo, not
                    // config-authored prose, so `output::one_line`'s sanitisation
                    // (which exists for strings a `veld.json` supplies) would only
                    // mangle the code fences it is made of.
                    emit(&line(t.body))
                }
            }
            None => {
                output::print_error(&unknown_topic_message(&name), json);
                1
            }
        },
    }
}

/// Whole kilobytes, rounded up. The reader is deciding whether to spend a call,
/// and the difference between 5K and 78K is the whole decision — a decimal place
/// is not.
fn kb(bytes: usize) -> String {
    format!("{}K", bytes.div_ceil(1024))
}

fn render_index() -> String {
    let mut s = String::new();
    s.push_str(&format!("{}\n\n", output::bold("Veld agent documentation")));
    let width = TOPICS.iter().map(|t| t.name.len()).max().unwrap_or(0);
    let size_width = TOPICS
        .iter()
        .map(|t| kb(t.body.len()).len())
        .max()
        .unwrap_or(0);
    for t in TOPICS {
        // The summary is wrapped in the source string, so collapse it back to one
        // line and let the reader's terminal (or the agent's context) do the rest.
        let summary = t.summary.split_whitespace().collect::<Vec<_>>().join(" ");
        // **The size is the point of this index, not decoration.** The reader has a
        // context budget and the topics are nowhere near uniform — `basics` is 5K,
        // `config` is 78K. Without a number, "read only the topic you need" is
        // advice an agent cannot act on, and two unlucky calls cost more than the
        // 100KB skill file this command exists to replace. It is free at the call
        // site: `body` is right here.
        s.push_str(&format!(
            "  {}  {}  {}\n",
            output::cyan(&output::pad_right(t.name, width)),
            output::dim(&output::pad_right(&kb(t.body.len()), size_width)),
            summary
        ));
    }
    s.push_str(&format!(
        "\nRead one with {}. Every document describes {} — not whatever veld the \
         docs were written against.\n",
        output::cyan("veld skills <topic>"),
        output::bold("this binary")
    ));
    s.push_str(
        "Live project state is not in here on purpose — ask for it when you need it: \
         `veld presets`, `veld nodes`, `veld status`, `veld config --files`.\n",
    );
    s
}

/// A wrong topic name is the one error this command can produce, so it carries
/// the whole index rather than sending the reader back for it — an agent that
/// guessed a name is one round-trip from the right one either way, and the list
/// is thirteen short words.
fn unknown_topic_message(name: &str) -> String {
    let names = TOPICS.iter().map(|t| t.name).collect::<Vec<_>>().join(", ");
    let mut msg = format!("Unknown skill topic `{}`.", output::one_line(name));
    if let Some(near) = closest(name) {
        msg.push_str(&format!(" Did you mean `{near}`?"));
    }
    msg.push_str(&format!("\n  Available: {names}"));
    msg
}

/// Suggest a topic for a near-miss, by the cheapest measure that catches the
/// misses that actually happen: a prefix, a substring, or a shared prefix of at
/// least four characters (`configuration` → `config`, `pane` → `ide-panes`).
/// Deliberately not an edit distance — a wrong guess here costs a wrong
/// suggestion, and the full list is printed underneath regardless.
fn closest(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    if lower.is_empty() {
        // Every name `starts_with("")`, so without this `veld skills ""` answers
        // "Did you mean `basics`?" — a suggestion derived from nothing.
        return None;
    }
    TOPICS
        .iter()
        .map(|t| t.name)
        .find(|n| n.starts_with(&lower) || lower.starts_with(*n) || n.contains(&lower))
        .or_else(|| {
            TOPICS
                .iter()
                .map(|t| t.name)
                .find(|n| shared_prefix(n, &lower) >= 4)
        })
}

fn shared_prefix(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

#[cfg(test)]
mod tests {
    use super::{TOPICS, closest, find, index_json, topic_json};

    /// The count is written out in prose in several places — AGENTS.md, the
    /// README, the shipped shell, the website, `llms-full.txt`. Nothing ties
    /// those to this array, and the checklist row in AGENTS.md actively invites
    /// adding a topic, so the fourteenth would silently falsify all of them.
    /// This is the tie: adding a topic fails here, with the list of what to edit.
    #[test]
    fn the_topic_count_matches_what_the_prose_claims() {
        assert_eq!(
            TOPICS.len(),
            13,
            "the number of topics changed. Update the word in AGENTS.md (Agent \
             Skills + the Documentation Checklist row), README.md, \
             skills/veld/SKILL.md, website/index.html and website/llms-full.txt, \
             then this number."
        );
    }

    #[test]
    fn every_topic_has_a_name_a_summary_and_a_body() {
        for t in TOPICS {
            assert!(!t.name.is_empty(), "a topic has no name");
            assert!(
                !t.summary.trim().is_empty(),
                "topic `{}` has no summary — it is what an agent reads to decide \
                 whether to spend the topic",
                t.name
            );
            assert!(
                t.body.trim().len() > 200,
                "topic `{}` is {} bytes — a stub in the index is worse than no entry, \
                 because an agent spends a call to find that out",
                t.name,
                t.body.trim().len()
            );
        }
    }

    /// A `.md` added beside the others but never added to `TOPICS` is compiled
    /// out entirely: never served, never reachable, and — because it is a
    /// tracked `.md` — still scanned by `tests/validate-doc-commands.py`, so it
    /// looks covered. The sibling shells test walks a directory for exactly this
    /// class of miss; this is the same walk on the other side.
    #[test]
    fn every_topic_document_on_disk_is_registered() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/skills");
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .expect("read the topic directory")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .collect();
        on_disk.sort();

        let mut registered: Vec<String> = TOPICS.iter().map(|t| t.name.to_owned()).collect();
        registered.sort();

        assert_eq!(
            on_disk,
            registered,
            "the documents in {} and the `TOPICS` array disagree. A file with no \
             entry is never served; an entry with no file does not compile.",
            dir.display()
        );
    }

    #[test]
    fn topic_names_are_unique_and_kebab_case() {
        let mut seen = std::collections::BTreeSet::new();
        for t in TOPICS {
            assert!(
                seen.insert(t.name),
                "two topics are called `{}` — `find` would only ever reach the first",
                t.name
            );
            assert!(
                t.name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "topic `{}` is not kebab-case; the name is typed by hand at a shell",
                t.name
            );
        }
    }

    /// A document moved out of `skills/veld/reference/` carries relative links
    /// (`[config](reference/config.md)`) that mean nothing once the text is a
    /// string in a binary — the reader has no filesystem to resolve them against.
    /// Nothing else notices: the link renders, and the agent follows it into a
    /// `Read` of a path that does not exist.
    ///
    /// An in-document `#anchor` is allowed and checked, because splitting one
    /// document into two is exactly how a working anchor becomes a dangling one —
    /// and it stays invisible, since a reader who does not find the section
    /// assumes they misread rather than that the link lied. (This caught a
    /// `#node-level-defaults` whose heading is `Node-level defaults (v3)`, wrong
    /// since before the move.)
    #[test]
    fn no_document_links_to_a_file_that_only_existed_in_the_repo() {
        for t in TOPICS {
            let anchors = heading_slugs(t.body);
            for target in markdown_link_targets(t.body) {
                if target.is_empty()
                    || target.starts_with("http://")
                    || target.starts_with("https://")
                    || target.starts_with("mailto:")
                {
                    continue;
                }
                if let Some(frag) = target.strip_prefix('#') {
                    assert!(
                        anchors.contains(frag),
                        "topic `{}` links to `#{frag}`, which matches no heading in it. \
                         Headings here: {:?}",
                        t.name,
                        anchors
                    );
                    continue;
                }
                panic!(
                    "topic `{}` links to `{}`, a repo path its reader cannot open. \
                     Point at another topic in prose instead — `see `veld skills config``.",
                    t.name, target
                );
            }
            // A Markdown link is not the only way to write a dead pointer, and
            // the narrower version of this test proved it: `Full rules:
            // `reference/config.md`` shipped in `gotchas.md` and was invisible
            // here, because it is a code span rather than a link.
            if let Some(path) = backticked_repo_paths(t.body).first() {
                panic!(
                    "topic `{}` names the repo path `{}` in a code span. Its reader \
                     has an installed binary and no checkout — name the topic \
                     (`veld skills <topic>`) or give an absolute https URL.",
                    t.name, path
                );
            }
        }
    }

    /// A code span that is a path into this repository — `docs/x.md`,
    /// `reference/config.md`, `skills/veld/SKILL.md`. Deliberately narrow: a
    /// span naming a file the *reader* has, such as their own `veld.json` or
    /// `scripts/veld/pr-badge.sh` that a topic tells them to write, is not one
    /// of these.
    fn backticked_repo_paths(body: &str) -> Vec<String> {
        const ROOTS: [&str; 5] = ["docs/", "reference/", "skills/", "crates/", "website/"];
        body.split('`')
            .skip(1)
            .step_by(2)
            .filter(|span| ROOTS.iter().any(|r| span.starts_with(r)))
            .map(|s| s.to_owned())
            .collect()
    }

    /// GitHub's heading-slug rules, which is what these anchors were written
    /// against: lowercase, punctuation dropped, spaces to hyphens.
    fn heading_slugs(body: &str) -> std::collections::BTreeSet<String> {
        body.lines()
            .filter_map(|l| l.trim_start().strip_prefix('#'))
            .map(|rest| rest.trim_start_matches('#').trim())
            .map(|title| {
                title
                    .chars()
                    .filter_map(|c| match c {
                        c if c.is_alphanumeric() => Some(c.to_ascii_lowercase()),
                        ' ' | '-' | '_' => Some('-'),
                        _ => None,
                    })
                    .collect()
            })
            .collect()
    }

    /// Minimal `[text](target)` scan. A dependency-free reader is enough here and
    /// keeps the gate in the same crate as the thing it guards.
    fn markdown_link_targets(body: &str) -> Vec<String> {
        let bytes: Vec<char> = body.chars().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == '[' {
                if let Some(close) = (i..bytes.len()).find(|&j| bytes[j] == ']') {
                    if close + 1 < bytes.len() && bytes[close + 1] == '(' {
                        if let Some(end) = (close + 2..bytes.len()).find(|&j| bytes[j] == ')') {
                            out.push(bytes[close + 2..end].iter().collect());
                            i = end + 1;
                            continue;
                        }
                    }
                }
            }
            i += 1;
        }
        out
    }

    /// The shells in `skills/` are the reason this module exists. If one grows a
    /// body again — or grows back the `!`-prefixed shell invocations that dumped
    /// `veld config` into every agent's context — the saving is gone and nothing
    /// else would say so.
    #[test]
    fn the_installed_skill_shells_stay_shells() {
        // Read the directory rather than listing the two files: AGENTS.md's
        // checklist row scopes this rule to `skills/*/SKILL.md`, and a hardcoded
        // pair would let a third shipped skill arrive exempt from a rule the
        // checklist says is enforced.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../skills")
            .canonicalize()
            .expect("the shipped skills directory");
        let mut shells: Vec<(String, String)> = std::fs::read_dir(&root)
            .expect("read skills/")
            .filter_map(Result::ok)
            .map(|e| e.path().join("SKILL.md"))
            .filter(|p| p.is_file())
            .map(|p| {
                let label = format!(
                    "skills/{}/SKILL.md",
                    p.parent()
                        .and_then(|d| d.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                );
                (label, std::fs::read_to_string(&p).expect("read SKILL.md"))
            })
            .collect();
        shells.sort();
        assert_eq!(
            shells.len(),
            2,
            "found {} shipped skill(s) under {}. Two is not arbitrary: README.md \
             says \"This installs two skills\" and AGENTS.md names both by path, \
             and neither is tied to this directory by anything else. A third is \
             fine — update those two files and this number.",
            shells.len(),
            root.display()
        );

        // The size rule above only looks at `SKILL.md`, so without this a
        // `skills/veld/reference/config.md` could come back beside a 3KB shell
        // and every gate would still pass — restoring the frozen-copy problem
        // this whole module exists to remove. That directory was emptied by this
        // change; nothing else would notice it refilling.
        for entry in std::fs::read_dir(&root).expect("read skills/").flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let strays: Vec<_> = std::fs::read_dir(&dir)
                .expect("read a skill directory")
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.file_name().is_none_or(|n| n != "SKILL.md"))
                .collect();
            assert!(
                strays.is_empty(),
                "a shipped skill carries only its SKILL.md shell; found {strays:?}. \
                 Documentation belongs in src/commands/skills/, served by \
                 `veld skills` — a file here is a copy that freezes the day a user \
                 installs it."
            );
        }
        for (label, text) in shells {
            assert!(
                !text.contains("\n!`"),
                "{label} runs a command at context-load time (a line starting with ``!` ``). \
                 That output enters every agent's context whether the task needs it or not; \
                 tell the agent to run the command when it has the question instead."
            );
            assert!(
                text.len() < 6_000,
                "{label} is {} bytes. A shell points at `veld skills`; it does not carry \
                 documentation, because a copy installed into somebody's project describes \
                 whichever veld they had the day they installed it.",
                text.len()
            );
        }
    }

    /// The two `--json` shapes are a contract stated in the clap doc comment on
    /// `Command::Skills`, and an agent branching on `.body` or `.summary` breaks
    /// on a rename with no compiler anywhere in the loop. Asserted against the
    /// same builders `run` uses rather than by shelling out, so this stays a
    /// unit test.
    #[test]
    fn the_json_shapes_are_what_the_flag_promises() {
        // The shipped builders, not a copy of them: the first version of this
        // test rebuilt the index inline and therefore kept passing while `run`
        // grew a field the doc comment did not mention.
        let index = index_json();
        let index = index.as_array().expect("array");
        assert_eq!(index.len(), TOPICS.len());
        let first = index[0].as_object().expect("object");
        let mut index_keys: Vec<_> = first.keys().map(String::as_str).collect();
        index_keys.sort_unstable();
        assert_eq!(index_keys, ["bytes", "name", "summary"]);
        assert_eq!(first["name"], "basics");
        assert!(first["summary"].is_string());
        assert!(
            first["bytes"].as_u64().expect("a number") > 0,
            "the index's whole job is letting a caller price a topic before fetching it"
        );
        assert!(
            first.get("body").is_none(),
            "the index must not carry bodies"
        );

        let doc = topic_json(find("runs").expect("runs"));
        let obj = doc.as_object().expect("object");
        let mut keys: Vec<_> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["body", "name", "summary"]);
        assert!(obj["body"].as_str().expect("string").starts_with('#'));
    }

    #[test]
    fn a_near_miss_gets_a_suggestion() {
        assert_eq!(closest("configuration"), Some("config"));
        assert_eq!(closest("pane"), Some("ide-panes"));
        assert_eq!(closest("basic"), Some("basics"));
        assert!(find("config").is_some());
        assert!(find("nope").is_none());
    }
}
