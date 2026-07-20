# veld-gateway container image.
#
# Built by the release pipeline from the SAME Linux binaries that ship in the
# release tarballs (no second compile): the workflow stages the per-arch
# binary under dist/<arch>/ and `docker buildx` picks the right one via
# TARGETARCH. Base is ubuntu:24.04 to match the glibc of the release build
# matrix (pinned to ubuntu-24.04 in release.yml) — a slimmer distroless base
# would carry an older glibc than the binaries were linked against.
#
# Configuration is env-var-first — a deployment needs zero files:
#
#   docker run -p 8080:8080 \
#     -e VELD_GATEWAY_DOMAIN=share.acme.internal \
#     -e VELD_GATEWAY_TOKEN=… \
#     -e VELD_GATEWAY_RELAYS=https://relay.acme.internal \
#     ghcr.io/prosperity-solutions/veld-gateway:latest
#
# Health endpoints: GET /livez (liveness) and GET /readyz (readiness), both
# answering on any Host; /healthz remains as a legacy liveness alias.
# Runs as a non-root user. The image
# sets VELD_GATEWAY_STATE_DIR, so a mounted volume there gives a stable iroh
# identity (and public URLs) across restarts — but the volume must be writable
# by the `veld` user or startup fails loudly (a root-owned mount is the usual
# cause). For truly ephemeral operation with no volume, that default is fine;
# to force ephemeral without a writable dir, unset VELD_GATEWAY_STATE_DIR.

FROM ubuntu:24.04

# ca-certificates: the gateway dials relays/hosts over TLS (iroh) and must
# verify public CAs.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /var/lib/veld-gateway --create-home veld

ARG TARGETARCH
COPY dist/${TARGETARCH}/veld-gateway /usr/local/bin/veld-gateway

# Ship license + third-party attribution in the image (MIT/Apache-2.0 etc.
# require the notices travel with the redistributed binary).
COPY LICENSE THIRD-PARTY-LICENSES.md /usr/share/doc/veld-gateway/

USER veld
ENV VELD_GATEWAY_STATE_DIR=/var/lib/veld-gateway
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/veld-gateway"]
