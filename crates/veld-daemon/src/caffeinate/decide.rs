//! The keep-awake state machine, with no I/O in it.
//!
//! Everything in this module is a pure function over values. That is not tidiness:
//! the side-effecting half of keep-awake spawns a process, talks to a privileged
//! helper over a socket and can spend fifteen seconds doing it, so a state machine
//! entangled with it can only be tested by holding a real machine awake. The
//! sibling module `veld-helper`'s `sleep` reached the same conclusion from the
//! other direction — a `SleepSetter` trait, because a test that ran `pmset` for
//! real would durably disable a developer's sleep.
//!
//! # Two reasons, not one flag
//!
//! The machine can be held awake because **a human asked** and because **a share
//! is live**, at the same time, and the two are not interchangeable:
//!
//! - they expire independently, so the hold lasts until the later of them is done;
//! - they may hold *different* things — an automatic hold never asks the
//!   privileged helper for anything, on either power source, because
//!   `pmset disablesleep` is a durable system setting and there is no press behind
//!   an automatic hold to justify writing one;
//! - "off" means off for both, but only a human can say it.
//!
//! Collapsing them into one flag looks smaller and is wrong in a specific,
//! measurable way: a user who set a short manual hold and *then* started sharing
//! would end up with less coverage than one who did nothing at all, because the
//! share could not extend a hold that already existed.
//!
//! # The automatic half is capped per *episode*, not per share
//!
//! A sharing **episode** starts when the hosted-share count goes from zero to
//! non-zero and ends when it returns to zero. Its clock also restarts when the
//! power source changes under it, because the cap that applies is the one for the
//! source you are actually on — otherwise starting a share on mains and then
//! unplugging would buy the mains allowance on battery, which is the one thing the
//! split settings exist to prevent.
//!
//! Within an episode the deadline is
//! `min(clock_started_at + cap_for_this_source, latest live share expiry)`. Binding
//! it to the shares is what stops this being a second, independent timer racing the
//! share reaper: shares already expire on their own (peer 2h, web 1h by default),
//! so the cap is a **ceiling** that normally never binds rather than a countdown
//! that usually fires first.
//!
//! Reaching the cap **opts the episode out**. Without that the hold would re-arm on
//! the next tick and the cap would be a lie; with it, "at most N minutes while
//! sharing" is literally what happens. The opt-out clears when the episode ends —
//! when sharing actually stops — and at no other time, which is also what makes
//! *"Let this machine sleep"* work: a human switching the hold off while a share is
//! live must not have it come straight back.

use chrono::{DateTime, Duration, Utc};
use veld_core::db::KeepAwakePrefs;

use super::power::{Power, PowerSource};

/// Why the machine is being held awake. Both may hold at once; neither holding is
/// how a session ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Reasons {
    /// A human asked. The inner `None` is "until I turn it off".
    pub manual: Option<Option<DateTime<Utc>>>,
    /// A share is live. Always bounded — see the module docs.
    pub sharing: Option<DateTime<Utc>>,
}

impl Reasons {
    pub fn is_empty(self) -> bool {
        self.manual.is_none() && self.sharing.is_none()
    }

    /// When the hold ends, or `None` for "until I turn it off".
    ///
    /// The *later* of the two, because each reason is independently sufficient.
    pub fn expires_at(self) -> Option<DateTime<Utc>> {
        match (self.manual, self.sharing) {
            // An unlimited manual hold swallows any sharing deadline.
            (Some(None), _) => None,
            (Some(Some(m)), Some(s)) => Some(m.max(s)),
            (Some(Some(m)), None) => Some(m),
            (None, s) => s,
        }
    }

    /// Drop whichever reasons have run out.
    ///
    /// Says nothing about *why* one ran out — telling a spent cap from a share's
    /// own expiry is `recompute`'s job, because only it knows the two deadlines
    /// that were combined to make this one, and only the cap opts an episode out.
    fn prune(&mut self, now: DateTime<Utc>) {
        if let Some(Some(deadline)) = self.manual {
            if now >= deadline {
                self.manual = None;
            }
        }
        if let Some(deadline) = self.sharing {
            if now >= deadline {
                self.sharing = None;
            }
        }
    }

    pub fn wire(self) -> &'static str {
        match (self.manual.is_some(), self.sharing.is_some()) {
            (true, true) => "both",
            (true, false) => "manual",
            (false, true) => "sharing",
            (false, false) => "none",
        }
    }
}

/// Why an episode stopped getting an automatic hold.
///
/// Two reasons, and they must not be collapsed: one is the cap doing its job,
/// the other is a person pressing a button. Plugging in restarts the first —
/// the mains allowance spends nothing, so refusing it would leave a charging
/// laptop asleep on the strength of a limit that no longer applies. It must
/// never restart the second, or a charger silently undoes "let this machine
/// sleep".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptOut {
    /// The allowance for this episode ran out.
    CapSpent,
    /// A human switched the hold off while sharing was live.
    UserSaidNo,
}

/// The live sharing episode's clock. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Episode {
    pub clock_started_at: DateTime<Utc>,
    /// The power source this clock was started for. A change restarts it.
    pub power: PowerSource,
}

/// What the daemon knows about hosted shares right now.
///
/// `count` is **hosted** shares only. A share you *joined* runs on somebody else's
/// machine and is not a reason to hold yours awake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShareFacts {
    pub count: usize,
    /// The latest expiry among live hosted shares.
    pub latest_expiry: Option<DateTime<Utc>>,
}

/// Everything the machine remembers between events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct State {
    pub reasons: Reasons,
    pub episode: Option<Episode>,
    /// This episode has had its automatic hold and will not get another, and
    /// why. See [`OptOut`].
    pub opted_out: Option<OptOut>,
    /// The inhibitor could not be started, so nothing is being held and retrying
    /// on every tick would achieve nothing but churn.
    ///
    /// Separate from [`Self::opted_out`] because the two mean opposite things to
    /// a reader: a spent cap is the feature working as configured, and this is
    /// the feature not working at all. Cleared when the episode ends, like the
    /// opt-out — a machine that could not spawn an inhibitor an hour ago may be
    /// able to now, and the next sharing is the natural moment to find out.
    pub spawn_failed: bool,
    /// Consecutive inhibitors that exited on their own straight after spawning.
    /// See `reap_if_dead`.
    pub deaths: u32,
}

/// How wide a hold is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// Idle sleep only. A shut lid still sleeps the machine.
    IdleOnly,
    /// Idle sleep and a shut lid.
    LidToo,
}

/// Why a shut lid is *not* covered, when it is not.
///
/// Three genuinely different answers, and the reason this is an enum rather than a
/// bool: the existing UI note for `NoHelper` tells the user to run
/// `veld setup privileged`, which is right when veld asked and could not get it and
/// actively wrong when veld never asked. Reporting a fault for something never
/// attempted is the failure this prevents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LidGap {
    /// Nobody pressed anything: an automatic hold never widens itself to the lid
    /// on battery. One click on a duration does.
    Automatic,
    /// `keepAwake.manualOnBattery` is off, so veld was told not to.
    Setting,
    /// veld asked and there is no privileged helper to ask, or it refused.
    NoHelper,
}

/// What the side-effecting half should be holding, given the state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub coverage: Coverage,
    /// Whether to hold the privileged `pmset disablesleep` lease. Only ever true
    /// for a hold a human asked for, on battery, on macOS, with the setting on.
    pub want_lease: bool,
    /// `None` when the lid is covered; otherwise why it is not.
    pub lid_gap: Option<LidGap>,
}

impl State {
    /// Apply "a human asked for a hold".
    ///
    /// Deliberately does not touch the sharing reason or the opt-out: picking a
    /// duration is a statement about the manual hold, not a way to buy back an
    /// automatic one that has already been spent.
    pub fn manual_start(&mut self, expires_at: Option<DateTime<Utc>>) {
        self.reasons.manual = Some(expires_at);
    }

    /// Apply "a human said let this machine sleep".
    ///
    /// Off means off: both reasons go, and while a share is live the episode is
    /// opted out so the automatic half cannot re-arm on the next tick and make the
    /// button look broken.
    pub fn manual_stop(&mut self, shares: ShareFacts) {
        self.reasons = Reasons::default();
        if shares.count > 0 {
            self.opted_out = Some(OptOut::UserSaidNo);
        }
    }

    /// Bring the automatic half up to date. The whole state machine lives here.
    ///
    /// Idempotent by construction — it reads the current facts rather than a
    /// remembered edge — which is what lets it be called from a detached task
    /// without an epoch or a sequence number to order concurrent runs.
    pub fn recompute(
        &mut self,
        now: DateTime<Utc>,
        prefs: &KeepAwakePrefs,
        power: Power,
        shares: ShareFacts,
    ) {
        self.reasons.prune(now);

        if shares.count == 0 {
            // The episode is over. This is the only place the opt-out clears, and
            // it is why "let this machine sleep" survives for exactly as long as
            // the sharing it was said during.
            self.episode = None;
            self.opted_out = None;
            self.spawn_failed = false;
            self.deaths = 0;
            self.reasons.sharing = None;
            return;
        }

        // An unmeasured reading picks **the episode's own** source, not the
        // battery it fell back to. Guarding only the clock restart left the other
        // half of the same bug: one timed-out `pmset` part-way through a mains
        // episode selected the 30-minute battery cap, decided the allowance was
        // already spent, and opted the episode out — with the mains-restart
        // clause unable to undo it, because the episode still says mains.
        let source = match (power.measured, self.episode) {
            (false, Some(episode)) => episode.power,
            _ => power.source,
        };
        let (enabled, cap_minutes) = match source {
            PowerSource::Mains => (prefs.sharing_on_power, prefs.sharing_on_power_minutes),
            PowerSource::Battery => (prefs.sharing_on_battery, prefs.sharing_on_battery_minutes),
        };

        // Plugging in after the battery allowance ran out restarts it, because the
        // mains hold is the one that spends nothing — refusing it would leave a
        // charging laptop asleep on the strength of a limit that no longer
        // applies.
        //
        // Only a **spent cap**, never a person: a charger must not undo "let this
        // machine sleep". And note honestly what this does *not* bound — plugging
        // in and then out again is a measured source change, which starts a fresh
        // episode and a fresh battery allowance by the ordinary rule. The
        // asymmetry here buys the user-said-no case, not a limit on charger
        // cycling; bounding that would need a budget across episodes, which is a
        // different feature and is written down as such in the module docs.
        if self.opted_out == Some(OptOut::CapSpent)
            && power.measured
            && power.source == PowerSource::Mains
            && self
                .episode
                .is_some_and(|e| e.power == PowerSource::Battery)
        {
            self.opted_out = None;
            self.episode = None;
        }

        if !enabled || self.opted_out.is_some() || self.spawn_failed {
            self.reasons.sharing = None;
            // The episode's clock is forgotten while the switch is off so that
            // turning it back on — or plugging in, when only the other source is
            // enabled — starts a fresh allowance rather than resuming a clock that
            // ran while nothing was being held.
            if !enabled {
                self.episode = None;
            }
            return;
        }

        // Start the clock, or restart it because the power source changed. A
        // change is a deliberate act by the person holding the laptop, and the cap
        // that applies afterwards is the one for the source they moved to.
        let episode = match self.episode {
            // An *unmeasured* source never restarts the clock: a probe that timed
            // out is not somebody unplugging a laptop, and treating it as one let
            // a flaky `pmset` reset the allowance on every flap.
            Some(existing) if existing.power == source || !power.measured => existing,
            _ => Episode {
                clock_started_at: now,
                power: source,
            },
        };
        self.episode = Some(episode);

        let cap_deadline = episode.clock_started_at + Duration::minutes(cap_minutes);
        let deadline = match shares.latest_expiry {
            Some(share_deadline) => cap_deadline.min(share_deadline),
            None => cap_deadline,
        };

        if now >= deadline {
            self.reasons.sharing = None;
            // Only the *cap* opts the episode out. Reaching the shares' own expiry
            // means they are about to be reaped, which will end the episode and
            // clear everything anyway — recording an opt-out for it would be
            // recording a decision nobody made.
            if now >= cap_deadline {
                self.opted_out = Some(OptOut::CapSpent);
            }
            return;
        }

        self.reasons.sharing = Some(deadline);
    }

    /// What to hold, given the reasons and where the power is coming from.
    pub fn plan(&self, prefs: &KeepAwakePrefs, power: Power) -> Option<Plan> {
        if self.reasons.is_empty() {
            return None;
        }
        let manual = self.reasons.manual.is_some();

        // The privileged lease is for a hold a **human asked for**, on macOS,
        // with the setting on — and deliberately **not** conditioned on the
        // power source. The source is learned on a tick; a lid slam is not. A
        // manual hold on mains whose owner shuts the lid and pulls the charger
        // suspends the machine before any tick could notice, so a lease taken
        // only once battery is *observed* is a lease taken too late. Asking on
        // mains costs nothing beyond a lease the helper's watchdog reverts
        // anyway, and it is what the pre-existing behaviour did.
        //
        // `manual` is the whole of what keeps the feature's central rule true:
        // an automatic hold never reaches this.
        let want_lease = manual && cfg!(target_os = "macos") && prefs.manual_on_battery;

        // On mains the widest hold is free: `caffeinate -s` is valid on AC power
        // only, needs no privileged helper and writes nothing durable, and
        // Linux's lid inhibitor is unprivileged on either source.
        if power.source == PowerSource::Mains {
            return Some(Plan {
                coverage: Coverage::LidToo,
                want_lease,
                lid_gap: None,
            });
        }

        // On battery, covering a shut lid is the one thing that costs something —
        // a durable macOS setting, or a real inhibitor holding a discharging
        // laptop open. Only a human can buy it.
        if !manual {
            return Some(Plan {
                coverage: Coverage::IdleOnly,
                want_lease: false,
                lid_gap: Some(LidGap::Automatic),
            });
        }
        // macOS only, because that is the whole of what the setting means: "never
        // write `pmset disablesleep`". Linux has no privileged half to decline —
        // its lid inhibitor is unprivileged on either power source — so honouring
        // the flag there would narrow a manual hold for a reason that does not
        // exist on the platform, and the settings dialog does not even render the
        // row there to explain it.
        if cfg!(target_os = "macos") && !prefs.manual_on_battery {
            return Some(Plan {
                coverage: Coverage::IdleOnly,
                want_lease: false,
                lid_gap: Some(LidGap::Setting),
            });
        }
        Some(Plan {
            coverage: Coverage::LidToo,
            want_lease,
            lid_gap: None,
        })
    }
}

/// What the live inhibitor is actually holding, for [`lid_state`].
///
/// The session's own fields, lifted out so the derivation below can be tested
/// without a real child process — which is why it went untested and shipped a
/// bug that reported a fault on every Linux laptop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveHold {
    pub coverage: Coverage,
    /// Whether a privileged lease was *wanted*. Distinct from whether one is
    /// held: only a lease that was asked for and refused is a fault.
    pub wanted_lease: bool,
    /// Whether it is held right now, per the renewal task's flag.
    pub lease_held: bool,
}

/// Does a shut lid keep this machine awake right now, and if not, why not.
///
/// Answered from the **running child** rather than from the plan. The two
/// disagree for one tick after the power source changes — `apply` respawns on a
/// coverage change, and until it has, the plan says the lid is covered while a
/// narrow inhibitor is what is actually holding. Answering from the plan there
/// prints "covers a closed lid" for a hold that does not, which this module's
/// docs call worse than no status at all.
pub fn lid_state(plan: Option<Plan>, live: Option<LiveHold>) -> (bool, Option<LidGap>) {
    let gap = match (plan.map(|p| p.lid_gap), live) {
        (Some(Some(gap)), _) => Some(gap),
        // The lease was wanted and is not held: the helper did not come through.
        // The `wanted_lease` test is what keeps this the only path that points a
        // user at `veld setup privileged` — asking merely whether the machine is
        // on battery reported a fault on every Linux laptop, where no lease is
        // ever wanted because the unprivileged inhibitor already covers the lid.
        (Some(None), Some(l)) if l.wanted_lease && !l.lease_held => Some(LidGap::NoHelper),
        _ => None,
    };
    let covered = live.is_some_and(|l| l.coverage == Coverage::LidToo) && gap.is_none();
    (covered, gap)
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Duration, TimeZone as _, Utc};
    use veld_core::db::KeepAwakePrefs;

    use super::super::power::{Power, PowerSource};
    use super::{Coverage, LidGap, LiveHold, OptOut, Plan, ShareFacts, State, lid_state};

    fn t(minute: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 14, 12, 0, 0).unwrap() + Duration::minutes(minute)
    }

    fn prefs() -> KeepAwakePrefs {
        KeepAwakePrefs {
            sharing_on_power: true,
            sharing_on_power_minutes: 120,
            sharing_on_battery: true,
            sharing_on_battery_minutes: 30,
            manual_on_battery: true,
        }
    }

    fn mains() -> Power {
        Power {
            source: PowerSource::Mains,
            measured: true,
            has_battery: true,
        }
    }

    fn battery() -> Power {
        Power {
            source: PowerSource::Battery,
            measured: true,
            has_battery: true,
        }
    }

    /// What a timed-out `pmset` produces: reads as battery, but is not evidence
    /// that anybody unplugged anything.
    fn unmeasured() -> Power {
        Power {
            source: PowerSource::Battery,
            measured: false,
            has_battery: true,
        }
    }

    fn sharing(count: usize) -> ShareFacts {
        ShareFacts {
            count,
            // Far enough out that the cap is what binds, unless a test says otherwise.
            latest_expiry: Some(t(10_000)),
        }
    }

    fn none() -> ShareFacts {
        ShareFacts::default()
    }

    #[test]
    fn a_share_arms_the_hold_and_the_cap_is_the_one_for_this_power_source() {
        let mut state = State::default();
        state.recompute(t(0), &prefs(), mains(), sharing(1));
        assert_eq!(state.reasons.sharing, Some(t(120)));

        let mut state = State::default();
        state.recompute(t(0), &prefs(), battery(), sharing(1));
        assert_eq!(state.reasons.sharing, Some(t(30)));
    }

    #[test]
    fn the_deadline_never_outlives_the_shares_that_justify_it() {
        // The point of binding to the shares: the cap is a ceiling, not a second
        // timer racing the share reaper.
        let mut state = State::default();
        let shares = ShareFacts {
            count: 1,
            latest_expiry: Some(t(45)),
        };
        state.recompute(t(0), &prefs(), mains(), shares);
        assert_eq!(state.reasons.sharing, Some(t(45)));
    }

    #[test]
    fn the_last_share_ending_drops_the_automatic_hold_and_clears_the_opt_out() {
        let mut state = State::default();
        state.recompute(t(0), &prefs(), mains(), sharing(1));
        state.opted_out = Some(OptOut::CapSpent);

        state.recompute(t(5), &prefs(), mains(), none());
        assert_eq!(state.reasons.sharing, None);
        assert!(state.episode.is_none());
        assert!(state.opted_out.is_none());
    }

    #[test]
    fn reaching_the_cap_opts_the_episode_out_so_the_hold_cannot_re_arm() {
        // Without the opt-out the next tick re-arms and "at most 30 minutes" is a
        // lie. This is the test that pins the cap being real.
        let mut state = State::default();
        state.recompute(t(0), &prefs(), battery(), sharing(1));
        assert!(state.reasons.sharing.is_some());

        state.recompute(t(30), &prefs(), battery(), sharing(1));
        assert_eq!(state.reasons.sharing, None);
        assert!(state.opted_out.is_some());

        state.recompute(t(31), &prefs(), battery(), sharing(1));
        assert_eq!(state.reasons.sharing, None);
    }

    #[test]
    fn a_share_expiring_is_not_an_opt_out() {
        // The distinction the cap arm depends on: the shares' own expiry ends the
        // episode a moment later anyway, so recording a decision nobody made would
        // suppress the *next* episode for no reason.
        let mut state = State::default();
        let shares = ShareFacts {
            count: 1,
            latest_expiry: Some(t(20)),
        };
        state.recompute(t(0), &prefs(), mains(), shares);
        state.recompute(t(20), &prefs(), mains(), shares);
        assert_eq!(state.reasons.sharing, None);
        assert!(state.opted_out.is_none());
    }

    #[test]
    fn a_second_share_extends_the_hold_but_never_past_the_episode_cap() {
        let mut state = State::default();
        state.recompute(t(0), &prefs(), battery(), sharing(1));
        // A second share arrives 20 minutes in. The episode clock still started at
        // t(0), so the cap is still t(30) — not t(50).
        state.recompute(t(20), &prefs(), battery(), sharing(2));
        assert_eq!(state.reasons.sharing, Some(t(30)));
    }

    #[test]
    fn unplugging_narrows_the_cap_and_restarts_the_clock() {
        // The maintainer's chosen rule, and the reason the battery cap cannot be
        // bypassed by starting a share on mains: the clock restarts, so you get a
        // fresh battery-length window rather than either the mains allowance or an
        // instant drop.
        let mut state = State::default();
        state.recompute(t(0), &prefs(), mains(), sharing(1));
        assert_eq!(state.reasons.sharing, Some(t(120)));

        state.recompute(t(45), &prefs(), battery(), sharing(1));
        assert_eq!(state.reasons.sharing, Some(t(75)));
    }

    #[test]
    fn plugging_in_widens_the_cap_and_restarts_the_clock() {
        let mut state = State::default();
        state.recompute(t(0), &prefs(), battery(), sharing(1));
        state.recompute(t(10), &prefs(), mains(), sharing(1));
        assert_eq!(state.reasons.sharing, Some(t(130)));
    }

    #[test]
    fn switching_the_source_off_drops_the_hold_and_the_other_source_still_works() {
        let mut prefs = prefs();
        prefs.sharing_on_battery = false;
        let mut state = State::default();

        state.recompute(t(0), &prefs, battery(), sharing(1));
        assert_eq!(state.reasons.sharing, None);

        // Plugging in must give the full mains allowance from that moment, not the
        // remainder of a clock that ran while nothing was held.
        state.recompute(t(40), &prefs, mains(), sharing(1));
        assert_eq!(state.reasons.sharing, Some(t(160)));
    }

    #[test]
    fn off_means_off_for_as_long_as_the_sharing_it_was_said_during() {
        let mut state = State::default();
        state.recompute(t(0), &prefs(), mains(), sharing(1));

        state.manual_stop(sharing(1));
        assert!(state.reasons.is_empty());

        // The next tick must not bring it straight back — that is a button that
        // does not work.
        state.recompute(t(1), &prefs(), mains(), sharing(1));
        assert_eq!(state.reasons.sharing, None);

        // …and it comes back for the *next* share.
        state.recompute(t(2), &prefs(), mains(), none());
        state.recompute(t(3), &prefs(), mains(), sharing(1));
        assert_eq!(state.reasons.sharing, Some(t(123)));
    }

    #[test]
    fn switching_off_when_nothing_is_shared_does_not_opt_anything_out() {
        let mut state = State::default();
        state.manual_start(Some(t(60)));
        state.manual_stop(none());
        assert!(state.opted_out.is_none());
    }

    #[test]
    fn a_manual_hold_and_a_share_hold_together_and_the_later_one_wins() {
        // The case a single flag gets wrong: a short manual hold must not stop the
        // share extending the machine's wakefulness past it.
        let mut state = State::default();
        state.manual_start(Some(t(10)));
        state.recompute(t(0), &prefs(), mains(), sharing(1));
        assert_eq!(state.reasons.expires_at(), Some(t(120)));

        // The manual reason running out leaves the sharing one holding.
        state.recompute(t(10), &prefs(), mains(), sharing(1));
        assert_eq!(state.reasons.manual, None);
        assert_eq!(state.reasons.sharing, Some(t(120)));
        assert!(!state.reasons.is_empty());
    }

    #[test]
    fn an_unlimited_manual_hold_outlives_every_share() {
        let mut state = State::default();
        state.manual_start(None);
        state.recompute(t(0), &prefs(), mains(), sharing(1));
        assert_eq!(state.reasons.expires_at(), None);

        state.recompute(t(500), &prefs(), mains(), none());
        assert_eq!(state.reasons.sharing, None);
        assert!(!state.reasons.is_empty());
    }

    #[test]
    fn an_automatic_hold_never_asks_for_the_privileged_lease() {
        // The load-bearing rule of the whole feature. `pmset disablesleep` is
        // durable, and there is no press behind an automatic hold to justify one.
        let mut state = State::default();
        state.recompute(t(0), &prefs(), battery(), sharing(1));
        let plan = state.plan(&prefs(), battery()).expect("a hold");
        assert!(!plan.want_lease);
        assert_eq!(plan.coverage, Coverage::IdleOnly);
        assert_eq!(plan.lid_gap, Some(LidGap::Automatic));
    }

    #[test]
    fn on_mains_even_an_automatic_hold_covers_a_shut_lid() {
        // Free there: `caffeinate -s` is valid on AC power only, so it needs no
        // privileged helper and writes nothing durable.
        let mut state = State::default();
        state.recompute(t(0), &prefs(), mains(), sharing(1));
        let plan = state.plan(&prefs(), mains()).expect("a hold");
        assert_eq!(plan.coverage, Coverage::LidToo);
        assert!(!plan.want_lease);
        assert_eq!(plan.lid_gap, None);
    }

    #[test]
    fn a_manual_hold_on_battery_asks_for_the_lease_unless_told_not_to() {
        let mut state = State::default();
        state.manual_start(Some(t(60)));

        let plan = state.plan(&prefs(), battery()).expect("a hold");
        assert_eq!(plan.coverage, Coverage::LidToo);
        assert_eq!(plan.want_lease, cfg!(target_os = "macos"));
        assert_eq!(plan.lid_gap, None);

        let mut off = prefs();
        off.manual_on_battery = false;
        let plan = state.plan(&off, battery()).expect("a hold");
        assert_eq!(plan.coverage, Coverage::IdleOnly);
        assert!(!plan.want_lease);
        // Not `NoHelper`: veld was told not to ask, so telling the user to install
        // a privileged helper would be advice for a problem they do not have.
        assert_eq!(plan.lid_gap, Some(LidGap::Setting));
    }

    #[test]
    fn a_manual_reason_widens_a_hold_that_sharing_armed() {
        // One click on a duration is how the automatic hold's lid gap is bought,
        // so the plan has to change the moment the manual reason exists.
        let mut state = State::default();
        state.recompute(t(0), &prefs(), battery(), sharing(1));
        assert_eq!(
            state.plan(&prefs(), battery()).expect("a hold").coverage,
            Coverage::IdleOnly
        );

        state.manual_start(Some(t(60)));
        assert_eq!(
            state.plan(&prefs(), battery()).expect("a hold").coverage,
            Coverage::LidToo
        );
    }

    #[test]
    fn a_linux_manual_hold_on_battery_is_not_a_missing_helper() {
        // The bug this seam exists for. Linux wants no lease *by construction*
        // (its lid inhibitor is unprivileged), so `lease_held` is always false —
        // and a gap derived from "on battery and no lease" told every Linux
        // laptop to run `veld setup privileged` while its lid was already
        // covered.
        let live = LiveHold {
            coverage: Coverage::LidToo,
            wanted_lease: false,
            lease_held: false,
        };
        let plan = Plan {
            coverage: Coverage::LidToo,
            want_lease: false,
            lid_gap: None,
        };
        assert_eq!(lid_state(Some(plan), Some(live)), (true, None));
    }

    #[test]
    fn a_lease_that_was_wanted_and_refused_is_the_one_reported_fault() {
        let live = LiveHold {
            coverage: Coverage::LidToo,
            wanted_lease: true,
            lease_held: false,
        };
        let plan = Plan {
            coverage: Coverage::LidToo,
            want_lease: true,
            lid_gap: None,
        };
        assert_eq!(
            lid_state(Some(plan), Some(live)),
            (false, Some(LidGap::NoHelper))
        );
    }

    #[test]
    fn a_plan_that_outran_its_child_does_not_claim_the_lid() {
        // One tick after plugging in: the plan says mains/lid-covered, the
        // running inhibitor is still the narrow one. The status must report the
        // child, not the intention.
        let live = LiveHold {
            coverage: Coverage::IdleOnly,
            wanted_lease: false,
            lease_held: false,
        };
        let plan = Plan {
            coverage: Coverage::LidToo,
            want_lease: false,
            lid_gap: None,
        };
        assert_eq!(lid_state(Some(plan), Some(live)), (false, None));
    }

    #[test]
    fn nothing_held_covers_no_lid() {
        assert_eq!(lid_state(None, None), (false, None));
    }

    #[test]
    fn plugging_in_after_the_battery_cap_ran_out_restarts_it() {
        // Refusing would leave a *charging* laptop asleep on the strength of a
        // limit that no longer applies, and the mains hold spends nothing.
        let mut state = State::default();
        state.recompute(t(0), &prefs(), battery(), sharing(1));
        state.recompute(t(30), &prefs(), battery(), sharing(1));
        assert!(state.opted_out.is_some());

        state.recompute(t(31), &prefs(), mains(), sharing(1));
        assert!(state.opted_out.is_none());
        assert_eq!(state.reasons.sharing, Some(t(151)));
    }

    #[test]
    fn unplugging_after_the_mains_cap_ran_out_does_not_restart_it() {
        // The asymmetry is the point: clearing the opt-out in this direction
        // would let somebody cycle the charger for unlimited battery allowances,
        // which is the whole thing the battery cap exists to bound.
        let mut state = State::default();
        state.recompute(t(0), &prefs(), mains(), sharing(1));
        state.recompute(t(120), &prefs(), mains(), sharing(1));
        assert!(state.opted_out.is_some());

        state.recompute(t(121), &prefs(), battery(), sharing(1));
        assert!(state.opted_out.is_some());
        assert_eq!(state.reasons.sharing, None);
    }

    #[test]
    fn a_machine_that_cannot_hold_is_not_told_its_allowance_is_used_up() {
        // `spawn_failed` and `opted_out` must stay separate: one is the feature
        // working as configured, the other is it not working, and the UI says
        // different things for them.
        let mut state = State::default();
        state.spawn_failed = true;
        state.recompute(t(0), &prefs(), mains(), sharing(1));
        assert_eq!(state.reasons.sharing, None);
        assert!(state.opted_out.is_none());

        // Cleared with the episode, so the next sharing tries again.
        state.recompute(t(1), &prefs(), mains(), none());
        assert!(!state.spawn_failed);
    }

    #[test]
    fn a_failed_power_reading_does_not_restart_the_clock() {
        // The bug this guards: a mains machine with a flaky probe flapped
        // mains→unknown→mains, restarting the episode clock each time, so the
        // 120-minute cap never accumulated and the hold never ended.
        let mut state = State::default();
        state.recompute(t(0), &prefs(), mains(), sharing(1));
        assert_eq!(state.reasons.sharing, Some(t(120)));

        state.recompute(t(10), &prefs(), unmeasured(), sharing(1));
        // The clock still belongs to the measured mains reading that started it.
        assert_eq!(state.episode.expect("an episode").clock_started_at, t(0));
    }

    #[test]
    fn a_charger_does_not_undo_let_this_machine_sleep() {
        // The two opt-outs must not be collapsed: plugging in restarts a spent
        // *cap*, and must never restart a hold a person switched off — a button
        // the charger undoes is a button that does not work.
        let mut state = State::default();
        state.recompute(t(0), &prefs(), battery(), sharing(1));
        state.manual_stop(sharing(1));
        assert_eq!(state.opted_out, Some(OptOut::UserSaidNo));

        state.recompute(t(5), &prefs(), mains(), sharing(1));
        assert_eq!(state.opted_out, Some(OptOut::UserSaidNo));
        assert_eq!(state.reasons.sharing, None);
    }

    #[test]
    fn an_unmeasured_reading_does_not_pick_the_other_sources_cap() {
        // Guarding only the clock restart left the other half of the same bug: a
        // timed-out probe part-way through a mains episode selected the
        // 30-minute battery cap and declared the allowance already spent.
        let mut state = State::default();
        state.recompute(t(0), &prefs(), mains(), sharing(1));
        state.recompute(t(45), &prefs(), unmeasured(), sharing(1));
        assert!(state.opted_out.is_none());
        assert_eq!(state.reasons.sharing, Some(t(120)));
    }

    #[test]
    fn no_reasons_is_no_plan() {
        assert!(State::default().plan(&prefs(), mains()).is_none());
    }

    #[test]
    fn recompute_is_idempotent_so_concurrent_callers_converge() {
        // The property that lets the daemon fire this from a detached task with no
        // epoch or sequence number: it reads the facts rather than an edge.
        let mut once = State::default();
        once.recompute(t(0), &prefs(), mains(), sharing(1));

        let mut thrice = State::default();
        for _ in 0..3 {
            thrice.recompute(t(0), &prefs(), mains(), sharing(1));
        }
        assert_eq!(once, thrice);
    }
}
