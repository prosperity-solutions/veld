//! Which version a build reports — shared by `build.rs` (which decides it) and
//! the library's tests (which pin the rule). Kept free of anything but `std`,
//! because a build script cannot use the crate it builds.

/// The `git describe` arguments that find the newest release tag reachable from
/// `HEAD`: `vX.Y.Z` only, never a prerelease (`v1.2.3-rc.1`) or a non-release
/// tag that happens to start with `v` and a digit.
pub const DESCRIBE_RELEASE_TAG: &[&str] = &[
    "describe",
    "--tags",
    "--abbrev=0",
    "--match",
    "v[0-9]*.[0-9]*.[0-9]*",
    "--exclude",
    "*-*",
];

/// The version a build reports: the newer of `Cargo.toml`'s (`pkg`) and the newest
/// release tag (`tag`, with or without its `v`), falling back to `pkg` whenever
/// the tag is missing or not `x.y.z`.
pub fn reported_version(pkg: &str, tag: Option<&str>) -> String {
    match tag.map(|t| t.trim_start_matches('v')) {
        Some(tag) if newer(tag, pkg) => tag.to_owned(),
        _ => pkg.to_owned(),
    }
}

/// `a > b` for `x.y.z` versions; anything that does not parse is never newer.
fn newer(a: &str, b: &str) -> bool {
    matches!((parts(a), parts(b)), (Some(a), Some(b)) if a > b)
}

fn parts(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.split('.');
    let mut next = || it.next()?.parse::<u64>().ok();
    let parsed = (next()?, next()?, next()?);
    it.next().is_none().then_some(parsed)
}

#[cfg(test)]
mod tests {
    use super::reported_version;

    #[test]
    fn a_checkout_reports_the_release_it_was_built_on() {
        // `Cargo.toml` on `main` is frozen; the newest tag is the release.
        assert_eq!(reported_version("16.89.3", Some("v17.2.0")), "17.2.0");
        assert_eq!(reported_version("16.89.3", Some("16.90.0")), "16.90.0");
    }

    #[test]
    fn a_release_build_keeps_its_stamped_version() {
        // The release stamps the *next* version, which is newer than every tag.
        assert_eq!(reported_version("17.3.0", Some("v17.2.0")), "17.3.0");
        assert_eq!(reported_version("17.3.0", None), "17.3.0");
    }

    #[test]
    fn numbers_compare_as_numbers_and_junk_never_wins() {
        assert_eq!(reported_version("16.9.0", Some("v16.10.0")), "16.10.0");
        assert_eq!(reported_version("16.89.3", Some("v16.89.3")), "16.89.3");
        assert_eq!(reported_version("16.89.3", Some("v99.0.0-rc.1")), "16.89.3");
        assert_eq!(reported_version("16.89.3", Some("backup-pr253")), "16.89.3");
        assert_eq!(reported_version("16.89.3", Some("v99.0")), "16.89.3");
    }
}
