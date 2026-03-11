# 1.0.0 (2026-03-11)


### Bug Fixes

* add CI diagnostics and show setup errors on failure ([42763dc](https://github.com/prosperity-solutions/veld/commit/42763dc836976ee7436bc7d8f55361d29fcf5932))
* address release-please review findings ([a33db70](https://github.com/prosperity-solutions/veld/commit/a33db70e90e0db8847cfefc15613915bcd59a969))
* align add_route args with helper protocol and clean up CI ([a9441b0](https://github.com/prosperity-solutions/veld/commit/a9441b0ba3f7356b47e9685ec4d67b5759f8c87a))
* configure Caddy storage dir to avoid read-only filesystem error ([7ffbd49](https://github.com/prosperity-solutions/veld/commit/7ffbd49c9c779c41efac99b42b334fcee40f9645))
* correct Caddy download URL — use 'mac' OS name and v2.11.2 ([8ed4c78](https://github.com/prosperity-solutions/veld/commit/8ed4c78771025abed11ef15c0fa547c88c23ee42))
* ensure Caddy starts during setup and orchestration ([67addce](https://github.com/prosperity-solutions/veld/commit/67addceb9536a13d33408ceb827664a009496bdc))
* fix CI integration tests and setup implementation ([5728d0e](https://github.com/prosperity-solutions/veld/commit/5728d0e14dba1f531613c83ce5ec63670e6c5a00))
* health check directly on port instead of through Caddy HTTPS ([27c9aa1](https://github.com/prosperity-solutions/veld/commit/27c9aa1a69e6261aa9308757d0fc982cd4146a22))
* make Caddy start idempotent and always ensure base config ([8ff84f9](https://github.com/prosperity-solutions/veld/commit/8ff84f968f52a4e1dbce501daaa7a19d67bbf586))
* replace macos-13 with cross-compilation and enforce Node.js 24 ([64fd6c9](https://github.com/prosperity-solutions/veld/commit/64fd6c9428ef786da26bb4b6fd1deb988e2ae3bc))
* use --resolve in integration test curl to bypass DNS ([1b1703c](https://github.com/prosperity-solutions/veld/commit/1b1703cb878a80c4b4fabf18d2a7610fab4f4b7d))
* use mkcert CA with Caddy internal TLS instead of wildcard certs ([09fe45c](https://github.com/prosperity-solutions/veld/commit/09fe45c900eb1285d02b31812f2f803d0c0c5cd7))


### Features

* add JSON schema validation to CI ([d1724ee](https://github.com/prosperity-solutions/veld/commit/d1724ee478aad859bc9956dd7cb43756bafc2f8c))
* implement Veld v1 — local dev environment orchestrator ([8529f13](https://github.com/prosperity-solutions/veld/commit/8529f138164e846629a0875db1447647d07de924))
