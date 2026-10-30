//! The parts of Veld OS, one module per part: its clap tree, its dispatch, and
//! its half of the update gate. A command belongs to the part that owns what it
//! acts on — see `docs/ship.md` → *Adding a command*.

pub mod agent;
pub mod ide;
pub mod network;
pub mod orchestrator;
pub mod runner;
pub mod self_;
