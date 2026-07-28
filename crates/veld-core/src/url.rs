use std::collections::HashMap;
use std::path::Path;

use crate::variables::{self, VariableError};

// ---------------------------------------------------------------------------
// Slugification
// ---------------------------------------------------------------------------

/// Convert an arbitrary string into a DNS-safe label.
///
/// # Contract
/// - Output alphabet: `[a-z0-9-]`
/// - Never starts or ends with `-`
/// - Maximum 48 characters (stricter than DNS's 63-char limit, to leave
///   room for composition in URL templates like `{service}.{run}.{project}.localhost`)
/// - Returns empty string if input contains no ASCII alphanumeric characters
/// - Idempotent: `slugify(slugify(x)) == slugify(x)` for all `x`
///
/// # Examples
/// ```
/// # use veld_core::url::slugify;
/// assert_eq!(slugify("feature/auth-flow"), "feature-auth-flow");
/// assert_eq!(slugify("My App"), "my-app");
/// assert_eq!(slugify("release/1.2.3"), "release-1-2-3");
/// ```
pub fn slugify(input: &str) -> String {
    let mut slug = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else {
            slug.push('-');
        }
    }

    // Collapse consecutive dashes.
    let mut collapsed = String::with_capacity(slug.len());
    let mut prev_dash = false;
    for ch in slug.chars() {
        if ch == '-' {
            if !prev_dash {
                collapsed.push('-');
            }
            prev_dash = true;
        } else {
            collapsed.push(ch);
            prev_dash = false;
        }
    }

    // Strip leading/trailing dashes.
    let trimmed = collapsed.trim_matches('-');

    // Max 48 characters.
    if trimmed.len() > 48 {
        trimmed[..48].trim_end_matches('-').to_owned()
    } else {
        trimmed.to_owned()
    }
}

// ---------------------------------------------------------------------------
// Caddy route ids
// ---------------------------------------------------------------------------

/// Prefix of every per-node run route id. Reserved namespaces the helper must
/// never re-key: `veld-join-*` (share joins, whose ids the daemon holds in
/// memory and removes verbatim), `veld-mgmt-*` (an instance's management
/// route), and the base config's `veld-management` / `veld-sentinel`.
pub const RUN_ROUTE_PREFIX: &str = "veld-run-";

/// Extract the bare hostname from a node URL (`https://web.dev.app.localhost:18443`
/// → `web.dev.app.localhost`). Scheme, port and path are dropped.
///
/// Every route-removal site needs this anyway (to remove the DNS host), and
/// [`run_route_id`] is derived from it — so both sides of the route lifecycle
/// must agree on exactly one implementation. That is this one.
///
/// # Examples
/// ```
/// # use veld_core::url::hostname_of_url;
/// assert_eq!(hostname_of_url("https://web.dev.app.localhost:18443"), "web.dev.app.localhost");
/// assert_eq!(hostname_of_url("web.dev.app.localhost"), "web.dev.app.localhost");
/// ```
pub fn hostname_of_url(url: &str) -> &str {
    let no_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let no_path = no_scheme.split('/').next().unwrap_or(no_scheme);
    no_path.split(':').next().unwrap_or(no_path)
}

/// The Caddy route id for a node served at `hostname`.
///
/// Keyed by the **hostname**, not by the run name (which is unique per project
/// only, so two repos both on `main` collided — issue #170) and not by the
/// globally-unique `run_id` (which would mint a fresh id on every start, so a
/// crashed run's entry could never be overwritten by the next start of the same
/// environment and would linger in the helper's durable store, shadowing the
/// live route for that hostname).
///
/// The invariant this buys: **one hostname ⇒ one route id ⇒ one store entry.**
/// Caddy matches on host, so two entries for one hostname have no defined
/// winner; keying by hostname makes that state unrepresentable. Two hostnames
/// that are genuinely equal (two clones of one repo, same run name) do collide
/// here — deliberately, because they cannot both be served. `veld start`
/// refuses that case up front instead of silently overwriting.
///
/// The hash suffix is load-bearing: `slugify` truncates at 48 characters, so
/// long hostnames sharing a 40-character prefix would otherwise alias.
///
/// # Examples
/// ```
/// # use veld_core::url::run_route_id;
/// let id = run_route_id("web.dev.app.localhost");
/// assert!(id.starts_with("veld-run-web-dev-app-localhost-"));
/// assert_eq!(id, run_route_id("WEB.dev.app.localhost")); // hostnames are case-insensitive
/// ```
pub fn run_route_id(hostname: &str) -> String {
    use sha2::{Digest, Sha256};

    let lower = hostname.to_ascii_lowercase();
    let digest = Sha256::digest(lower.as_bytes());
    // 8 hex characters (32 bits) — enough to separate the handful of hostnames
    // one machine serves, short enough to keep the id readable in logs.
    let mut hash = String::with_capacity(8);
    for byte in &digest[..4] {
        use std::fmt::Write as _;
        let _ = write!(hash, "{byte:02x}");
    }

    let mut label = slugify(&lower);
    // slugify already caps at 48; trim further so id length stays predictable.
    if label.len() > 40 {
        label.truncate(40);
        label = label.trim_end_matches('-').to_owned();
    }
    if label.is_empty() {
        // No ASCII alphanumerics in the hostname at all — the hash alone still
        // identifies it uniquely.
        return format!("{RUN_ROUTE_PREFIX}{hash}");
    }
    format!("{RUN_ROUTE_PREFIX}{label}-{hash}")
}

/// The pre-#170 route id format, kept only so teardown can also delete a route
/// stored by an older helper.
///
/// The helper canonicalises its whole store to [`run_route_id`] on startup, so
/// this matters in one window: `veld update` installed new binaries but the *old*
/// helper is still running (see `veld update`'s warning for the modes where it
/// cannot restart itself). Without this, stopping such a run would leave its
/// legacy entry behind to shadow the hostname forever.
///
/// The mirror-image window is a **downgrade** (`VELD_VERSION=<older>`): an older
/// helper only ever removes the legacy id, so every `veld-run-*` entry this build
/// persisted becomes a store orphan it will replay but never clean up. Nothing
/// prunes it automatically — delete `caddy-data/veld-routes.json` (the helper
/// rebuilds it from the next `veld start`) if a downgrade leaves stale routes.
///
/// Only ever call it on a **removal** path. Deleting a legacy id before adding
/// ours would re-create #170's start collision, since the legacy id may belong
/// to another project's live run.
pub fn legacy_run_route_id(run_name: &str, node: &str, variant: &str) -> String {
    format!("veld-{run_name}-{node}-{variant}")
}

// ---------------------------------------------------------------------------
// Git helpers
// ---------------------------------------------------------------------------

/// Detect the current git branch, or return an empty string.
///
/// Returns the literal string `"HEAD"` when in detached HEAD state.
/// Callers that want a meaningful branch name should treat `"HEAD"` as empty.
pub fn detect_git_branch(project_root: &Path) -> String {
    git_output(project_root, &["rev-parse", "--abbrev-ref", "HEAD"])
}

/// Return `true` if `project_root` is inside a linked git worktree
/// (i.e. not the main worktree).
fn is_linked_worktree(project_root: &Path) -> bool {
    let git_dir = git_output(project_root, &["rev-parse", "--git-dir"]);
    let common_dir = git_output(project_root, &["rev-parse", "--git-common-dir"]);
    if git_dir.is_empty() || common_dir.is_empty() {
        return false;
    }
    // In a linked worktree, --git-dir points into .git/worktrees/<name>,
    // which differs from --git-common-dir (the main .git directory).
    let git_dir = std::fs::canonicalize(project_root.join(&git_dir));
    let common_dir = std::fs::canonicalize(project_root.join(&common_dir));
    match (git_dir, common_dir) {
        (Ok(g), Ok(c)) => g != c,
        _ => false,
    }
}

/// Run a git command and return its trimmed stdout, or an empty string.
fn git_output(project_root: &Path, args: &[&str]) -> String {
    std::process::Command::new("git")
        .args(args)
        .current_dir(project_root)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_owned())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Run name generation
// ---------------------------------------------------------------------------

/// Generate a default run name from the project context.
///
/// The result is always a valid DNS label (see [`slugify`]), except for the
/// petname fallback which uses the same `[a-z]-[a-z]` format.
///
/// # Cascade
///
/// 1. **Linked worktree folder name** (slugified) — if the project root is a
///    linked git worktree, the folder name was chosen deliberately by the user
///    (e.g. `myapp-feature-x`).
/// 2. **Git branch name** (slugified) — if in a git repo with a branch checked
///    out. Detached HEAD (`git rev-parse --abbrev-ref HEAD` returns literal
///    `"HEAD"`) is treated as "no branch" and falls through.
/// 3. **Project folder name** (slugified) — the directory containing `veld.json`.
/// 4. **Random petname** — two-word random name (e.g. `swift-falcon`) as a
///    last resort.
///
/// # Determinism
///
/// Steps 1–3 produce deterministic names for a given project+branch state.
/// This is intentional: [`crate::orchestrator::Orchestrator::cleanup_stale_run`]
/// tears down any existing run with the same name, so reusing a name replaces
/// the previous run rather than accumulating stale instances.
pub fn generate_run_name(project_root: &Path) -> String {
    // 1. If we're in a linked worktree, the folder name was chosen with intent.
    if is_linked_worktree(project_root) {
        if let Some(folder) = project_root.file_name().and_then(|n| n.to_str()) {
            let slugged = slugify(folder);
            if !slugged.is_empty() {
                return slugged;
            }
        }
    }

    // 2. Try git branch (skip detached HEAD — "HEAD" carries no meaning).
    let branch = detect_git_branch(project_root);
    if !branch.is_empty() && branch != "HEAD" {
        let slugged = slugify(&branch);
        if !slugged.is_empty() {
            return slugged;
        }
    }

    // 3. Try folder name.
    if let Some(folder) = project_root.file_name().and_then(|n| n.to_str()) {
        let slugged = slugify(folder);
        if !slugged.is_empty() {
            return slugged;
        }
    }

    // 4. Random petname.
    petname::petname(2, "-").unwrap_or_else(|| "default".to_owned())
}

// ---------------------------------------------------------------------------
// URL template resolution (cascade: variant > node > project > built-in)
// ---------------------------------------------------------------------------

/// Resolve the effective URL template for a given node+variant, using the
/// most specific override: variant > node > project.
pub fn resolve_url_template<'a>(
    project_template: &'a str,
    node_template: Option<&'a str>,
    variant_template: Option<&'a str>,
) -> &'a str {
    if let Some(t) = variant_template {
        return t;
    }
    if let Some(t) = node_template {
        return t;
    }
    project_template
}

// ---------------------------------------------------------------------------
// URL template evaluation
// ---------------------------------------------------------------------------

/// Build the complete URL for a node given the URL template and context values.
///
/// Template syntax uses `{var}` (not `${var}`) and supports `{a ?? b}` fallback.
pub fn evaluate_url_template(
    template: &str,
    values: &HashMap<String, String>,
) -> Result<String, VariableError> {
    variables::interpolate_url_template(template, values)
}

/// Check whether a hostname is a `.localhost` domain (RFC 6761).
pub fn is_localhost_domain(hostname: &str) -> bool {
    hostname == "localhost" || hostname.ends_with(".localhost")
}

/// Build the template variables map for a given node in a run.
///
/// All values are slugified for DNS safety. If an input (e.g. `run_name`) was
/// already produced by [`slugify`], the re-slugification is a no-op because
/// `slugify` is idempotent.
#[allow(clippy::too_many_arguments)]
pub fn build_url_template_values(
    service: &str,
    variant: &str,
    run_name: &str,
    project: &str,
    branch: &str,
    worktree: &str,
    username: &str,
    hostname: &str,
) -> HashMap<String, String> {
    let mut values = HashMap::new();
    values.insert("service".to_owned(), slugify(service));
    values.insert("variant".to_owned(), slugify(variant));
    values.insert("run".to_owned(), slugify(run_name));
    values.insert("project".to_owned(), slugify(project));
    values.insert("branch".to_owned(), slugify(branch));
    values.insert("worktree".to_owned(), slugify(worktree));
    values.insert("username".to_owned(), slugify(username));
    values.insert("hostname".to_owned(), slugify(hostname));
    values
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert that a string satisfies the slugify contract (valid DNS label fragment).
    fn assert_valid_slug(s: &str) {
        assert!(s.len() <= 48, "slug too long ({} chars): {:?}", s.len(), s);
        assert!(
            s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "slug contains invalid chars: {:?}",
            s
        );
        if !s.is_empty() {
            assert!(!s.starts_with('-'), "slug starts with dash: {:?}", s);
            assert!(!s.ends_with('-'), "slug ends with dash: {:?}", s);
        }
    }

    // -- slugify: basic transformations --

    #[test]
    fn test_slugify_passthrough() {
        assert_eq!(slugify("hello-world"), "hello-world");
        assert_eq!(slugify("main"), "main");
        assert_eq!(slugify("my-app"), "my-app");
        assert_eq!(slugify("abc123"), "abc123");
    }

    #[test]
    fn test_slugify_lowercase() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("UPPER"), "upper");
        assert_eq!(slugify("MiXeD"), "mixed");
    }

    #[test]
    fn test_slugify_branch_names() {
        assert_eq!(slugify("feature/auth-flow"), "feature-auth-flow");
        assert_eq!(
            slugify("feature/JIRA-1234-oauth"),
            "feature-jira-1234-oauth"
        );
        assert_eq!(slugify("bugfix/fix_login"), "bugfix-fix-login");
        assert_eq!(slugify("release/1.2.3"), "release-1-2-3");
        assert_eq!(slugify("refs/heads/main"), "refs-heads-main");
    }

    // -- slugify: edge cases --

    #[test]
    fn test_slugify_empty() {
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn test_slugify_all_non_alnum() {
        assert_eq!(slugify("---"), "");
        assert_eq!(slugify("////"), "");
        assert_eq!(slugify("..."), "");
        assert_eq!(slugify("@#$%"), "");
    }

    #[test]
    fn test_slugify_leading_trailing_dashes() {
        assert_eq!(slugify("-leading"), "leading");
        assert_eq!(slugify("trailing-"), "trailing");
        assert_eq!(slugify("-both-"), "both");
        assert_eq!(slugify("---padded---"), "padded");
    }

    #[test]
    fn test_slugify_consecutive_dashes() {
        assert_eq!(slugify("a--b"), "a-b");
        assert_eq!(slugify("--multi---dash--"), "multi-dash");
        assert_eq!(slugify("a///b"), "a-b");
        assert_eq!(slugify("a   b"), "a-b");
    }

    // -- slugify: unicode --

    #[test]
    fn test_slugify_unicode() {
        // Non-ASCII letters become dashes; trailing dashes are stripped.
        assert_eq!(slugify("café"), "caf");
        assert_eq!(slugify("naïve"), "na-ve");
        // Emoji-only input produces empty string.
        assert_eq!(slugify("🎉"), "");
        assert_eq!(slugify("hello🌍world"), "hello-world");
    }

    // -- slugify: truncation --

    #[test]
    fn test_slugify_truncation() {
        // 60 alphanumeric chars -> truncated to 48.
        let long = "a".repeat(60);
        let result = slugify(&long);
        assert_eq!(result.len(), 48);
        assert_eq!(result, "a".repeat(48));
    }

    #[test]
    fn test_slugify_truncation_trailing_dash() {
        // Build a string that, after slugification, has a dash at position 48.
        // "a{47}/" -> slugified becomes "a{47}-" (48 chars with trailing dash)
        // then trimmed to 47 "a"s.
        let input = format!("{}/x", "a".repeat(47));
        let result = slugify(&input);
        assert_valid_slug(&result);
        assert!(!result.ends_with('-'));
    }

    #[test]
    fn test_slugify_exactly_48() {
        let input = "a".repeat(48);
        assert_eq!(slugify(&input), input);
    }

    // -- slugify: idempotency --

    #[test]
    fn test_slugify_idempotent() {
        let long = "a".repeat(60);
        let inputs: Vec<&str> = vec![
            "feature/auth-flow",
            "Hello World",
            "release/1.2.3",
            "---padded---",
            &long,
            "café",
            "my-app",
            "",
        ];
        for input in inputs {
            let once = slugify(input);
            let twice = slugify(&once);
            assert_eq!(once, twice, "slugify is not idempotent for {:?}", input);
        }
    }

    // -- slugify: DNS validity on all outputs --

    #[test]
    fn test_slugify_dns_validity() {
        let long = "a".repeat(60);
        let inputs: Vec<&str> = vec![
            "feature/auth-flow",
            "Hello World",
            "release/1.2.3",
            "---",
            "",
            &long,
            "café",
            "--multi---dash--",
            "-leading",
        ];
        for input in inputs {
            assert_valid_slug(&slugify(input));
        }
    }

    // -- is_localhost_domain --

    #[test]
    fn test_is_localhost_domain() {
        assert!(is_localhost_domain("localhost"));
        assert!(is_localhost_domain("app.localhost"));
        assert!(is_localhost_domain(
            "frontend.my-feature.myproject.localhost"
        ));
        assert!(!is_localhost_domain("myapp.dev"));
        assert!(!is_localhost_domain("app.mycompany.dev"));
        assert!(!is_localhost_domain("notlocalhost"));
        assert!(!is_localhost_domain("foo.localhost.evil.com"));
        assert!(!is_localhost_domain(""));
    }

    // -- route ids --

    #[test]
    fn hostname_of_url_strips_scheme_port_and_path() {
        assert_eq!(
            hostname_of_url("https://web.dev.app.localhost:18443"),
            "web.dev.app.localhost"
        );
        assert_eq!(
            hostname_of_url("https://web.dev.app.localhost"),
            "web.dev.app.localhost"
        );
        assert_eq!(
            hostname_of_url("http://web.dev.app.localhost:3000/path"),
            "web.dev.app.localhost"
        );
        // Already bare (what the helper reads back out of a stored route).
        assert_eq!(hostname_of_url("app.localhost"), "app.localhost");
        assert_eq!(hostname_of_url(""), "");
    }

    #[test]
    fn route_id_separates_projects_that_share_a_run_name() {
        // #170: `veld-{run}-{node}-{variant}` made these two identical.
        let a = run_route_id("web.main.repo-a.localhost");
        let b = run_route_id("web.main.repo-b.localhost");
        assert_ne!(a, b);
        assert!(a.starts_with(RUN_ROUTE_PREFIX), "{a}");
    }

    #[test]
    fn route_id_is_stable_and_case_insensitive() {
        let host = "web.dev.app.localhost";
        assert_eq!(run_route_id(host), run_route_id(host));
        assert_eq!(run_route_id(host), run_route_id("WEB.DEV.APP.LOCALHOST"));
        // Pinned so a format change is a deliberate, reviewed act: every
        // existing install's persisted routes are keyed by this exact string
        // (the helper re-keys on startup, but only because it can recompute it).
        assert_eq!(
            run_route_id(host),
            "veld-run-web-dev-app-localhost-367906bb"
        );
    }

    #[test]
    fn route_id_hash_separates_hostnames_that_slug_identically() {
        // The reason the hash suffix exists: slugify truncates, so two long
        // hostnames sharing a 40-char prefix would otherwise collide.
        let a = "aaaaaaaaaa.bbbbbbbbbb.cccccccccc.dddddddddd.one.localhost";
        let b = "aaaaaaaaaa.bbbbbbbbbb.cccccccccc.dddddddddd.two.localhost";
        assert_eq!(slugify(a)[..40], slugify(b)[..40]);
        assert_ne!(run_route_id(a), run_route_id(b));
    }

    #[test]
    fn route_id_survives_a_hostname_with_no_alphanumerics() {
        let id = run_route_id("...");
        assert_eq!(id, format!("{RUN_ROUTE_PREFIX}ab5df625"));
        assert_ne!(id, run_route_id(".."));
    }

    #[test]
    fn route_id_never_lands_in_a_reserved_namespace() {
        // The helper's startup canonicalisation skips these prefixes, so a
        // generated run route id must never look like one of them.
        for host in [
            "join.app.localhost",
            "mgmt.app.localhost",
            "management.localhost",
            "sentinel.localhost",
        ] {
            let id = run_route_id(host);
            assert!(id.starts_with(RUN_ROUTE_PREFIX), "{id}");
            assert!(!id.starts_with("veld-join-"), "{id}");
            assert!(!id.starts_with("veld-mgmt-"), "{id}");
            assert_ne!(id, "veld-management");
            assert_ne!(id, "veld-sentinel");
        }
    }

    #[test]
    fn legacy_route_id_matches_the_pre_170_format() {
        // Teardown deletes this id too, so it must stay byte-identical to what
        // shipped before #170 — a stale entry is only removable by its old key.
        assert_eq!(
            legacy_run_route_id("main", "web", "local"),
            "veld-main-web-local"
        );
    }

    // -- generate_run_name (integration tests using real git) --

    /// Run a shell command in a directory; panics on failure.
    fn run_git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("git command failed to start");
        assert!(status.success(), "git {:?} failed in {:?}", args, dir);
    }

    #[test]
    fn test_run_name_non_git_uses_folder() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = generate_run_name(tmp.path());
        let folder = tmp.path().file_name().unwrap().to_str().unwrap();
        assert_eq!(result, slugify(folder));
    }

    #[test]
    fn test_run_name_git_uses_branch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        run_git(dir, &["init", "-b", "my-feature"]);
        run_git(dir, &["commit", "--allow-empty", "-m", "init"]);

        assert_eq!(generate_run_name(dir), "my-feature");
    }

    #[test]
    fn test_run_name_detached_head_uses_folder() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        run_git(dir, &["init", "-b", "some-branch"]);
        run_git(dir, &["commit", "--allow-empty", "-m", "init"]);
        run_git(dir, &["checkout", "--detach", "HEAD"]);

        let result = generate_run_name(dir);
        let folder = dir.file_name().unwrap().to_str().unwrap();
        // Should NOT be "head" — should fall through to folder name.
        assert_ne!(result, "head");
        assert_eq!(result, slugify(folder));
    }

    #[test]
    fn test_run_name_worktree_uses_worktree_folder() {
        let tmp = tempfile::TempDir::new().unwrap();
        let main_dir = tmp.path().join("main-repo");
        std::fs::create_dir(&main_dir).unwrap();
        run_git(&main_dir, &["init", "-b", "main"]);
        run_git(&main_dir, &["commit", "--allow-empty", "-m", "init"]);
        run_git(&main_dir, &["branch", "feature-x"]);

        let wt_dir = tmp.path().join("my-worktree");
        run_git(
            &main_dir,
            &["worktree", "add", wt_dir.to_str().unwrap(), "feature-x"],
        );

        let result = generate_run_name(&wt_dir);
        // Should use the worktree folder name, not the branch.
        assert_eq!(result, "my-worktree");
    }

    #[test]
    fn test_run_name_branch_with_slashes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        run_git(dir, &["init", "-b", "feature/auth-flow"]);
        run_git(dir, &["commit", "--allow-empty", "-m", "init"]);

        assert_eq!(generate_run_name(dir), "feature-auth-flow");
    }
}
