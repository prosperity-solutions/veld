use crate::output;

/// Print version information for all Veld binaries.
pub fn print_version() {
    let cli_version = env!("CARGO_PKG_VERSION");

    // The daemon and helper binaries share the workspace version. When those
    // crates expose a `VERSION` constant we can read it directly; for now we
    // use the same workspace version.
    let daemon_version = cli_version;
    let helper_version = cli_version;

    println!("{}", output::bold("Veld"));
    println!("  cli      {cli_version}");
    println!("  daemon   {daemon_version}");
    println!("  helper   {helper_version}");
}
