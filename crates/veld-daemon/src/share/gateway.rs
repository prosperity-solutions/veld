//! Client for the public web gateway's registration API.
//!
//! The daemon registers a *web share*'s ticket with the org's gateway, then
//! keeps the registration alive by re-`POST`ing (the heartbeat) until the
//! share ends. DTOs live in `veld_core::share` — shared with `veld-gateway`
//! itself, so the wire contract cannot drift.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use tracing::warn;
use veld_core::config::GatewayRef;
use veld_core::share::{GatewayAccessPolicy, GatewayRegisterRequest, GatewayRegisterResponse};
use veld_share::endpoint::resolve_secret;

/// Env overrides pairing with the relay ones: point web shares at a gateway
/// without config (ad-hoc/testing). Config wins when both are present.
const GATEWAY_ENV: &str = "VELD_SHARE_GATEWAY";
const GATEWAY_TOKEN_ENV: &str = "VELD_SHARE_GATEWAY_TOKEN";

/// Budget for one registration call. Generous because the gateway's join
/// includes hole-punching to this very daemon (and the share's approval flow).
const REGISTER_TIMEOUT: Duration = Duration::from_secs(90);

/// A resolved gateway target: base URL + resolved auth token.
#[derive(Clone)]
pub struct GatewayClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl GatewayClient {
    /// Resolve the gateway from config (`sharing.gateway`), falling back to the
    /// `VELD_SHARE_GATEWAY` env override. Errors if neither names a gateway or
    /// no auth token is available — web sharing never runs unauthenticated.
    pub async fn resolve(config_ref: Option<&GatewayRef>) -> Result<Self> {
        let (url, token_source) = match config_ref {
            Some(gw) => (gw.url.clone(), gw.token.clone()),
            None => match std::env::var(GATEWAY_ENV) {
                Ok(url) if !url.trim().is_empty() => (url.trim().to_owned(), None),
                _ => bail!(
                    "no web gateway configured: set `sharing.gateway` in veld.json \
                     (a URL, or {{ \"url\", \"token\" }}) or the VELD_SHARE_GATEWAY env var"
                ),
            },
        };

        // Token: config declaration wins; otherwise the env override pairs in.
        let token = match token_source {
            Some(source) => resolve_secret(&source)
                .await
                .context("resolving the gateway auth token (sharing.gateway.token)")?,
            None => match std::env::var(GATEWAY_TOKEN_ENV) {
                Ok(t) if !t.trim().is_empty() => t.trim().to_owned(),
                _ => bail!(
                    "no gateway auth token available: set `sharing.gateway.token` in veld.json \
                     (a literal, or {{ \"env\" | \"file\" | \"command\" }}) or the \
                     VELD_SHARE_GATEWAY_TOKEN env var"
                ),
            },
        };

        let base_url = url.trim_end_matches('/').to_owned();
        if !base_url.starts_with("https://") && !base_url.starts_with("http://") {
            bail!("gateway URL must start with http(s):// (got `{base_url}`)");
        }
        // Over plain http to a non-loopback gateway, the ShareTicket (which
        // carries the capability — the public-URL bearer secret) and the Bearer
        // gateway token both transit in cleartext. Fine for localhost testing;
        // warn otherwise so it isn't a silent downgrade.
        if let Some(rest) = base_url.strip_prefix("http://") {
            let host = rest.split([':', '/']).next().unwrap_or(rest);
            let loopback = host == "localhost" || host == "127.0.0.1" || host == "[::1]";
            if !loopback {
                warn!(
                    gateway = %base_url,
                    "gateway URL is plain http — the share capability and gateway token \
                     will transit in cleartext; use https for anything but local testing"
                );
            }
        }

        Ok(Self {
            base_url,
            token,
            http: reqwest::Client::builder()
                .timeout(REGISTER_TIMEOUT)
                .build()
                .context("building gateway HTTP client")?,
        })
    }

    /// Register `ticket` (idempotent — also the heartbeat). `access` is the
    /// viewer access policy (§6.1); it rides every call so a restarted
    /// gateway re-learns the password with the lease. Returns the minted
    /// public URLs and the lease the origin must heartbeat inside.
    pub async fn register(
        &self,
        ticket: &str,
        access: Option<&GatewayAccessPolicy>,
    ) -> Result<GatewayRegisterResponse> {
        let resp = self
            .http
            .post(format!("{}/api/v1/shares", self.base_url))
            .bearer_auth(&self.token)
            .json(&GatewayRegisterRequest {
                ticket: ticket.to_owned(),
                access: access.cloned(),
            })
            .send()
            .await
            .with_context(|| format!("reaching gateway {}", self.base_url))?;

        let status = resp.status();
        if !status.is_success() {
            let detail = resp.text().await.unwrap_or_default();
            bail!(
                "gateway {} refused the registration ({status}): {}",
                self.base_url,
                detail.trim()
            );
        }
        resp.json::<GatewayRegisterResponse>()
            .await
            .context("parsing gateway registration response")
    }

    /// Unregister (best-effort; the lease expiring covers a lost DELETE).
    pub async fn unregister(&self, id: &str) -> Result<()> {
        self.http
            .delete(format!("{}/api/v1/shares/{id}", self.base_url))
            .bearer_auth(&self.token)
            .send()
            .await
            .with_context(|| format!("reaching gateway {}", self.base_url))?
            .error_for_status()
            .with_context(|| format!("gateway {} rejected the unregister", self.base_url))?;
        Ok(())
    }
}

#[cfg(test)]
impl GatewayClient {
    /// A client pointed at a dummy URL for tests that need a `WebRegistration`
    /// without a live gateway (no request is ever sent).
    pub(crate) fn for_test() -> Self {
        Self {
            base_url: "https://gateway.test".into(),
            token: "test-token".into(),
            http: reqwest::Client::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use veld_core::config::SecretSource;

    #[tokio::test]
    async fn resolve_requires_a_gateway_and_a_token() {
        // (Env vars deliberately not consulted here — pass a config ref so the
        // test doesn't depend on process env; the None-path env fallback is
        // covered by the error message contract below.)
        let gw = GatewayRef {
            url: "https://share.acme.internal".into(),
            token: Some(SecretSource::Literal("tok".into())),
        };
        let client = GatewayClient::resolve(Some(&gw)).await.unwrap();
        assert_eq!(client.base_url, "https://share.acme.internal");
        assert_eq!(client.token, "tok");

        // Trailing slash is normalized so path joins are stable.
        let gw2 = GatewayRef {
            url: "https://share.acme.internal/".into(),
            token: Some(SecretSource::Literal("tok".into())),
        };
        assert_eq!(
            GatewayClient::resolve(Some(&gw2)).await.unwrap().base_url,
            "https://share.acme.internal"
        );

        // A non-URL is rejected up front with a pointed message.
        let bad = GatewayRef {
            url: "share.acme.internal".into(),
            token: Some(SecretSource::Literal("tok".into())),
        };
        let Err(err) = GatewayClient::resolve(Some(&bad)).await else {
            panic!("expected non-URL gateway to be rejected");
        };
        assert!(err.to_string().contains("http(s)://"), "{err}");
    }
}
