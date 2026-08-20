//! What a browser gets when it opens a veld URL.
//!
//! Caddy issues and renews the certificate for every hostname veld serves, and
//! veld has always taken that on trust. The helper's watchdog asks Caddy's admin
//! API whether it is alive; `veld doctor` asks the keychain whether the *CA* is
//! trusted. Neither question notices the one failure a user actually hits — a
//! leaf certificate that expired and was never renewed, which Chrome refuses
//! with `ERR_CERT_DATE_INVALID` while every check veld had stayed green.
//!
//! That is not hypothetical. A Caddy whose certificate-maintenance goroutine had
//! stopped served a 12-hour leaf for 29 hours past its expiry; the watchdog only
//! restarted it a day later, when its admin API happened to go silent for an
//! unrelated reason. Reloading the config would not have helped either — see
//! [`crate::tls_health`]'s user in the helper for why only a restart renews.
//!
//! So this module asks the browser's question instead: complete a TLS handshake
//! on the HTTPS port and read the validity dates off the certificate that comes
//! back.

use std::net::SocketAddr;
use std::time::{Duration, SystemTime};

use x509_cert::der::Decode;

/// Loopback, so fast — a slow connect here means nothing is listening.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Bound on the whole probe. Mirrors the helper's Caddy admin client: a
/// half-dead Caddy can accept a connection and never answer, and a watchdog that
/// waits forever on it is a watchdog that has stopped watching.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Path the base Caddy config answers itself, on every hostname, with no
/// upstream involved (`veld-sentinel`). Using it keeps this probe a statement
/// about Caddy and its certificate, not about whatever the route proxies to.
const SENTINEL_PATH: &str = "/__veld_sentinel__";

/// The smallest "renewal is overdue" margin, whatever the certificate's
/// lifetime. A certificate short enough that a sixth of its life is under half an
/// hour is one veld does not issue; the floor keeps the threshold meaningful if it
/// ever meets one.
const RENEWAL_OVERDUE_FLOOR: Duration = Duration::from_secs(30 * 60);

/// The state of the certificate veld's HTTPS port is serving right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TlsHealth {
    /// A browser accepts this certificate today, and for `expires_in` longer.
    /// `lifetime` is the certificate's whole `notBefore → notAfter` span, which
    /// is what makes "renewal is overdue" answerable — see
    /// [`Self::renewal_is_overdue`].
    Valid {
        expires_in: Duration,
        lifetime: Duration,
    },
    /// Expired `expired_for` ago. Browsers refuse the page.
    Expired { expired_for: Duration },
    /// `notBefore` is in the future by `valid_in`. Browsers refuse this exactly
    /// as they refuse an expired one, and veld used to call it healthy: a machine
    /// whose clock stepped backwards (a VM snapshot, an NTP correction, a dead
    /// RTC battery) is served certificates it will not accept, while every check
    /// reads green. **Not** a renewal fault — see [`Self::renewal_is_overdue`].
    NotYetValid { valid_in: Duration },
    /// No TLS answer at all — nothing listening, or a Caddy that never replied.
    /// Distinct from a certificate verdict on purpose: this is the *liveness*
    /// watchdog's business, and acting on it here would restart Caddy for a
    /// reason that has nothing to do with certificates.
    Unreachable { detail: String },
    /// The handshake completed but the certificate could not be read.
    Unreadable { detail: String },
}

impl TlsHealth {
    /// Would a browser load a veld URL right now?
    pub fn serves_browsers(&self) -> bool {
        matches!(self, Self::Valid { .. })
    }

    /// Is renewal provably not happening — expired, or so deep into the window
    /// where Caddy should already have replaced this certificate that nothing
    /// can be replacing it?
    ///
    /// The threshold is a *fraction of the certificate's own lifetime*, not a
    /// fixed span, because that is how certmagic decides: it renews once
    /// `RenewalWindowRatio` (default `1/3`) of the lifetime is left, computed
    /// from `notAfter - notBefore`
    /// (`certmagic/certificates.go`'s `currentlyInRenewalWindow`). Half that
    /// window — a sixth of the lifetime — is comfortably past due whatever the
    /// lifetime is: 28 hours for a 7-day leaf, 2 hours for one of the 12-hour
    /// leaves an install still holds until its certificates roll over.
    ///
    /// A fixed threshold cannot do both. One hour spends 55 of the 56 hours of
    /// slack a 7-day leaf buys, and a fixed 24 hours would call *every* 12-hour
    /// certificate overdue the moment it was issued — restarting Caddy, on every
    /// existing install, every cooldown, forever.
    ///
    /// `false` for [`Self::Unreachable`] and [`Self::Unreadable`]: a probe that
    /// learned nothing is not evidence of a certificate problem. `false` for
    /// [`Self::NotYetValid`] too, and deliberately: the clock is wrong, and a
    /// restart would only reissue another certificate the clock rejects.
    pub fn renewal_is_overdue(&self) -> bool {
        match self {
            Self::Valid {
                expires_in,
                lifetime,
            } => *expires_in < (*lifetime / 6).max(RENEWAL_OVERDUE_FLOOR),
            Self::Expired { .. } => true,
            Self::NotYetValid { .. } | Self::Unreachable { .. } | Self::Unreadable { .. } => false,
        }
    }

    /// How bad this verdict is, for picking one out of several hosts' worth.
    ///
    /// A certificate fault outranks a probe that learned nothing, so one
    /// unreachable hostname can never hide another's expired certificate; and
    /// `Valid` ranks last so it never wins over something worth acting on.
    fn rank(&self) -> u8 {
        match self {
            Self::Expired { .. } => 5,
            Self::NotYetValid { .. } => 4,
            Self::Valid { .. } if self.renewal_is_overdue() => 3,
            Self::Unreadable { .. } => 2,
            Self::Unreachable { .. } => 1,
            Self::Valid { .. } => 0,
        }
    }
}

/// The worst verdict among several hosts, and the host it belongs to.
///
/// Empty input is `None` — a caller with no hostnames to check has learned
/// nothing, which must not be reported as health.
///
/// Note what this is *not* good for: deciding that things are **well**. A single
/// hostname that answers nothing outranks every healthy one, by design, so that
/// it cannot hide a fault — which also means this never returns `Valid` while any
/// one host is unreachable. Ask [`all_healthy`] instead when the question is
/// whether the fault has cleared.
pub fn worst(verdicts: Vec<(String, TlsHealth)>) -> Option<(String, TlsHealth)> {
    verdicts.into_iter().max_by_key(|(_, health)| health.rank())
}

/// Whether a set of verdicts says the certificates are *fine*: at least one host
/// was actually read and found good, and not one is faulted.
///
/// Deliberately not `!worst().renewal_is_overdue()`. A permanently unissuable
/// hostname — one route Caddy cannot get a certificate for — makes `worst()`
/// `Unreachable` for as long as it exists, and a caller that read recovery out of
/// `worst()` would never see the fault clear again: it would sit in its
/// give-up state for the rest of the process's life, including through a later
/// expiry it should have acted on.
pub fn all_healthy(verdicts: &[(String, TlsHealth)]) -> bool {
    let mut any_good = false;
    for (_, health) in verdicts {
        if health.renewal_is_overdue() || matches!(health, TlsHealth::NotYetValid { .. }) {
            return false;
        }
        any_good |= matches!(health, TlsHealth::Valid { .. });
    }
    any_good
}

/// Read the certificate veld's HTTPS port serves for the management hostname.
///
/// **Certificate verification is deliberately off.** The point of the probe is
/// to read a certificate a browser would *reject*, and verifying would turn that
/// exact state into a connection error with no certificate attached. Nothing
/// here trusts what it reads: the verdict comes from the certificate's own
/// validity dates, no request body is consumed, and the connection is dropped.
/// Do not copy this client for anything that fetches data.
///
/// [`crate::instance::MANAGEMENT_HOST`] is the one hostname guaranteed to have a
/// certificate — it is in the base Caddy config whether or not a run is up — so
/// it is the right *canary*, and the only sound choice for a caller that knows no
/// other names. It is **not** sufficient on its own: every run hostname carries
/// its own leaf with its own dates, issued when that run first started, so a run
/// URL can be expired while this one is still valid. A caller holding the list of
/// hostnames it serves should probe them all and combine with [`worst`]; the
/// helper's certificate watchdog does exactly that.
pub async fn probe(https_port: u16) -> TlsHealth {
    probe_host(crate::instance::MANAGEMENT_HOST, https_port).await
}

/// [`probe`] for one specific hostname veld serves.
pub async fn probe_host(host: &str, https_port: u16) -> TlsHealth {
    let client = match reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .danger_accept_invalid_certs(true)
        .tls_info(true)
        // Loopback only, and that has to be enforced twice. `.resolve` pins the
        // address, but reqwest reads `HTTPS_PROXY`/`ALL_PROXY` from the
        // environment by default (`auto_sys_proxy`) and its matcher exempts no
        // address, loopback included — so on a machine with a proxy configured
        // this probe would hand the request to it and, with verification off,
        // read back *the proxy's* certificate as veld's. A tool built to stop
        // veld reporting a certificate it has not looked at must not do that.
        .no_proxy()
        .resolve(host, SocketAddr::from(([127, 0, 0, 1], https_port)))
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            return TlsHealth::Unreadable {
                detail: format!("could not build probe client: {e}"),
            };
        }
    };

    let url = format!("https://{host}:{https_port}{SENTINEL_PATH}");
    let response = match client.get(&url).send().await {
        Ok(response) => response,
        Err(e) => {
            return TlsHealth::Unreachable {
                detail: innermost(&e),
            };
        }
    };

    // The HTTP status is not consulted: this route is Caddy's own static
    // response, and even a surprising status would still have been delivered
    // over the handshake whose certificate is the question.
    let Some(der) = response
        .extensions()
        .get::<reqwest::tls::TlsInfo>()
        .and_then(|info| info.peer_certificate().map(<[u8]>::to_vec))
    else {
        return TlsHealth::Unreadable {
            detail: "TLS connection carried no peer certificate".to_owned(),
        };
    };

    classify(&der, SystemTime::now())
}

/// The verdict for one DER-encoded certificate, as of `now`.
///
/// Split out so the arithmetic is testable against a real certificate without a
/// server to serve it.
///
/// Only the leaf is examined, and that is sufficient rather than merely
/// convenient: Caddy's internal issuer clamps every certificate it signs to its
/// intermediate's own `NotAfter`, so a leaf can never outlive the chain above it.
/// An expired intermediate therefore always presents as an expired leaf. (Which
/// is as well — the TLS layer hands back the leaf alone.)
fn classify(der: &[u8], now: SystemTime) -> TlsHealth {
    let cert = match x509_cert::Certificate::from_der(der) {
        Ok(cert) => cert,
        Err(e) => {
            return TlsHealth::Unreadable {
                detail: format!("could not parse the served certificate: {e}"),
            };
        }
    };

    let validity = cert.tbs_certificate().validity();
    let not_before = validity.not_before.to_system_time();
    let not_after = validity.not_after.to_system_time();

    // Both ends, because a browser checks both. `notBefore` in the future is the
    // clock-went-backwards case, and Caddy's internal issuer does not backdate
    // (its leaves start at the moment of issuance), so it is reachable on any
    // machine that resumed with a stale clock.
    if let Ok(valid_in) = not_before.duration_since(now) {
        if !valid_in.is_zero() {
            return TlsHealth::NotYetValid { valid_in };
        }
    }
    match not_after.duration_since(now) {
        // A malformed certificate whose `notAfter` precedes its `notBefore` would
        // otherwise report a nonsense lifetime; `duration_since` failing that way
        // means the lifetime is unknowable, so treat the certificate as
        // unreadable rather than guess.
        Ok(expires_in) => match not_after.duration_since(not_before) {
            Ok(lifetime) => TlsHealth::Valid {
                expires_in,
                lifetime,
            },
            Err(_) => TlsHealth::Unreadable {
                detail: "certificate's notAfter precedes its notBefore".to_owned(),
            },
        },
        // `duration_since` fails precisely when `not_after` is in the past, and
        // the error carries by how much.
        Err(past) => TlsHealth::Expired {
            expired_for: past.duration(),
        },
    }
}

/// The last error in a `reqwest` chain — the sentence that says what actually
/// went wrong, rather than the URL it went wrong on.
fn innermost(e: &reqwest::Error) -> String {
    let mut source: &dyn std::error::Error = e;
    while let Some(next) = source.source() {
        source = next;
    }
    source.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    /// A real leaf as Caddy's internal issuer produces it: `veld.localhost`,
    /// signed by "Veld Local CA - ECC Intermediate", valid
    /// 2026-08-20T01:42:15Z → 2026-08-20T13:42:15Z (the 12-hour default this
    /// change moves away from). Kept as base64 rather than a binary fixture so
    /// what is committed is readable; there is no private key here, and every
    /// test below supplies its own `now`, so the fixture does not age out.
    const LEAF_DER_BASE64: &str = concat!(
        "MIIBujCCAWCgAwIBAgIQCZMr6/dmoJmq6jbBngj9BDAKBggqhkjOPQQDAjArMSkwJwYDVQQDEyBW",
        "ZWxkIExvY2FsIENBIC0gRUNDIEludGVybWVkaWF0ZTAeFw0yNjA4MjAwMTQyMTVaFw0yNjA4MjAx",
        "MzQyMTVaMAAwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAAQfca154btyWS7RszClb+eDvGF0wsRr",
        "TtjuUl3mWV+jZNr9s5M4vVlyzSnC14qZRZgqm/lwFgf5vNpN366H15eko4GQMIGNMA4GA1UdDwEB",
        "/wQEAwIHgDAdBgNVHSUEFjAUBggrBgEFBQcDAQYIKwYBBQUHAwIwHQYDVR0OBBYEFBtc3bpazgb3",
        "bHtgybY7JSfZUeboMB8GA1UdIwQYMBaAFHNz230wiUWWEXs+qQkQqcSCdPNbMBwGA1UdEQEB/wQS",
        "MBCCDnZlbGQubG9jYWxob3N0MAoGCCqGSM49BAMCA0gAMEUCIQCrie71bDA7ZMLmxpgBdDaCVXX5",
        "G5shamqXX7XojHxacwIgAMbId3No/mpWcvI8kM0vTgKgjtwaOZTqLbpgffjQbKY=",
    );

    /// `notAfter` of the fixture, in seconds since the epoch.
    const FIXTURE_NOT_AFTER: u64 = 1_787_233_335;

    fn fixture() -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(LEAF_DER_BASE64)
            .expect("fixture is valid base64")
    }

    fn at(unix_secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(unix_secs)
    }

    /// The fixture's own span, which the threshold is a fraction of.
    const FIXTURE_LIFETIME: Duration = Duration::from_secs(12 * 3600);

    #[test]
    fn a_certificate_with_hours_left_is_valid() {
        let health = classify(&fixture(), at(FIXTURE_NOT_AFTER - 5 * 3600));
        assert_eq!(
            health,
            TlsHealth::Valid {
                expires_in: Duration::from_secs(5 * 3600),
                lifetime: FIXTURE_LIFETIME,
            }
        );
        assert!(health.serves_browsers());
        assert!(!health.renewal_is_overdue());
    }

    /// The clock-went-backwards case: a certificate issued *after* now. Browsers
    /// refuse it exactly as they refuse an expired one, and reporting it as valid
    /// is the all-green blindness this module exists to end.
    #[test]
    fn a_future_dated_certificate_is_not_yet_valid_rather_than_valid() {
        // The fixture starts 12h before its notAfter; stand an hour before that.
        let health = classify(&fixture(), at(FIXTURE_NOT_AFTER - 13 * 3600));
        assert_eq!(
            health,
            TlsHealth::NotYetValid {
                valid_in: Duration::from_secs(3600)
            }
        );
        assert!(!health.serves_browsers());
        // A restart cannot fix a wrong clock — it would reissue a certificate the
        // clock rejects just the same — so this must never trigger the remedy.
        assert!(!health.renewal_is_overdue());
    }

    /// The threshold is a sixth of the certificate's *own* lifetime, because
    /// certmagic's renewal window is a third of it. A fixed span cannot serve both
    /// the 12-hour leaves installs still hold and the 7-day ones veld now asks for.
    #[test]
    fn overdue_scales_with_the_certificates_own_lifetime() {
        // 12h leaf: renewal due with 4h left, so 2h left is provably late...
        let late_short = TlsHealth::Valid {
            expires_in: Duration::from_secs(2 * 3600 - 60),
            lifetime: Duration::from_secs(12 * 3600),
        };
        assert!(late_short.renewal_is_overdue());
        // ...and 3h left is not yet.
        let fine_short = TlsHealth::Valid {
            expires_in: Duration::from_secs(3 * 3600),
            lifetime: Duration::from_secs(12 * 3600),
        };
        assert!(!fine_short.renewal_is_overdue());

        // 7-day leaf: 28h is the line. A fixed 1h threshold would have waited
        // until 55 hours after renewal provably stopped.
        let week = Duration::from_secs(7 * 24 * 3600);
        assert!(
            TlsHealth::Valid {
                expires_in: Duration::from_secs(27 * 3600),
                lifetime: week
            }
            .renewal_is_overdue()
        );
        assert!(
            !TlsHealth::Valid {
                expires_in: Duration::from_secs(29 * 3600),
                lifetime: week
            }
            .renewal_is_overdue()
        );
        // And a fixed 24h threshold would have called every 12-hour certificate
        // overdue the moment it was issued, restarting Caddy forever.
        assert!(
            !TlsHealth::Valid {
                expires_in: Duration::from_secs(12 * 3600),
                lifetime: Duration::from_secs(12 * 3600)
            }
            .renewal_is_overdue()
        );
    }

    /// One hostname that answers nothing must never hide another whose
    /// certificate has expired.
    #[test]
    fn the_worst_verdict_wins_across_hosts() {
        let expired = TlsHealth::Expired {
            expired_for: Duration::from_secs(60),
        };
        let valid = TlsHealth::Valid {
            expires_in: Duration::from_secs(6 * 24 * 3600),
            lifetime: Duration::from_secs(7 * 24 * 3600),
        };
        let unreachable = TlsHealth::Unreachable {
            detail: "refused".to_owned(),
        };

        let (host, health) = worst(vec![
            ("a.localhost".to_owned(), valid.clone()),
            ("b.localhost".to_owned(), unreachable.clone()),
            ("c.localhost".to_owned(), expired.clone()),
        ])
        .expect("three verdicts");
        assert_eq!(host, "c.localhost");
        assert_eq!(health, expired);

        // Nothing actionable: the probe that learned nothing still outranks a
        // healthy one, so the report says so rather than claiming health.
        let (host, health) = worst(vec![
            ("a.localhost".to_owned(), valid),
            ("b.localhost".to_owned(), unreachable.clone()),
        ])
        .expect("two verdicts");
        assert_eq!(host, "b.localhost");
        assert_eq!(health, unreachable);

        // No hostnames means nothing was learned, not that everything is fine.
        assert!(worst(vec![]).is_none());
    }

    #[test]
    fn a_certificate_past_not_after_is_expired_by_the_overshoot() {
        let health = classify(&fixture(), at(FIXTURE_NOT_AFTER + 29 * 3600));
        assert_eq!(
            health,
            TlsHealth::Expired {
                expired_for: Duration::from_secs(29 * 3600)
            }
        );
        assert!(!health.serves_browsers());
        assert!(health.renewal_is_overdue());
    }

    /// The window between "Caddy should have renewed this hours ago" and "a
    /// browser refuses it" is the one the watchdog exists to act in — it is
    /// still servable, and still a fault. For this 12-hour fixture the line sits
    /// at two hours (a sixth of its lifetime).
    #[test]
    fn a_certificate_deep_in_its_renewal_window_is_valid_but_overdue() {
        let health = classify(&fixture(), at(FIXTURE_NOT_AFTER - 2 * 3600 + 60));
        assert!(health.serves_browsers());
        assert!(health.renewal_is_overdue());
    }

    /// A minute the other side of the line is not yet evidence of anything — the
    /// threshold is a claim about Caddy's renewal window, so it must not fire
    /// early. Anchored on a real certificate, so the fraction is computed from
    /// dates Caddy actually issued rather than from constants.
    #[test]
    fn a_certificate_just_outside_the_margin_is_not_overdue() {
        let health = classify(&fixture(), at(FIXTURE_NOT_AFTER - 2 * 3600 - 60));
        assert!(health.serves_browsers());
        assert!(!health.renewal_is_overdue());
    }

    /// `worst` must not be used to conclude health, and `all_healthy` is why.
    /// One hostname Caddy can never issue for keeps `worst` at `Unreachable`
    /// forever; a watchdog that read recovery from that would stay in its give-up
    /// state for the rest of its uptime, through a later expiry it should act on.
    #[test]
    fn health_is_judged_over_the_whole_set_not_by_the_worst_verdict() {
        let valid = TlsHealth::Valid {
            expires_in: Duration::from_secs(6 * 24 * 3600),
            lifetime: Duration::from_secs(7 * 24 * 3600),
        };
        let unreachable = TlsHealth::Unreachable {
            detail: "no certificate for this name".to_owned(),
        };
        let set = vec![
            ("good.localhost".to_owned(), valid.clone()),
            ("never.localhost".to_owned(), unreachable),
        ];
        // The worst verdict is the unreachable one, and stays so...
        assert!(matches!(
            worst(set.clone()).expect("verdicts").1,
            TlsHealth::Unreachable { .. }
        ));
        // ...but nothing is *faulted*, and one host was read and is good.
        assert!(all_healthy(&set));

        // A real fault anywhere is not health, however many hosts are fine.
        let mut faulted = set;
        faulted.push((
            "expired.localhost".to_owned(),
            TlsHealth::Expired {
                expired_for: Duration::from_secs(60),
            },
        ));
        assert!(!all_healthy(&faulted));

        // Nothing read at all is not health either.
        assert!(!all_healthy(&[(
            "n.localhost".to_owned(),
            TlsHealth::Unreadable {
                detail: "x".to_owned()
            }
        )]));
        assert!(!all_healthy(&[]));

        // Nor is a certificate whose renewal has provably stopped.
        assert!(!all_healthy(&[(
            "late.localhost".to_owned(),
            TlsHealth::Valid {
                expires_in: Duration::from_secs(3600),
                lifetime: Duration::from_secs(7 * 24 * 3600),
            }
        )]));
    }

    #[test]
    fn garbage_is_unreadable_rather_than_expired() {
        let health = classify(b"not a certificate", SystemTime::now());
        assert!(matches!(health, TlsHealth::Unreadable { .. }));
        // Nothing was learned, so nothing is claimed: an unreadable probe must
        // never send the watchdog restarting Caddy.
        assert!(!health.renewal_is_overdue());
        assert!(!health.serves_browsers());
    }

    /// End-to-end over a real socket: something is listening, but it is not
    /// speaking TLS. The probe must report that it learned nothing rather than
    /// blaming the certificate — the whole watchdog hangs off that distinction.
    #[tokio::test]
    async fn a_port_that_is_not_speaking_tls_is_unreachable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        // Accept and drop: the handshake fails, deterministically, without
        // depending on a port being free by the time the probe connects.
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                drop(stream);
            }
        });

        let health = probe(port).await;
        assert!(
            matches!(health, TlsHealth::Unreachable { .. }),
            "expected Unreachable, got {health:?}"
        );
        assert!(!health.renewal_is_overdue());
    }

    #[test]
    fn an_unreachable_port_claims_nothing_about_the_certificate() {
        let health = TlsHealth::Unreachable {
            detail: "connection refused".to_owned(),
        };
        assert!(!health.renewal_is_overdue());
        assert!(!health.serves_browsers());
    }
}
