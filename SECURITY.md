# Security Policy

"Who's At The Door?" is a public game. Anyone can play, and the votes become
training data for a doorbell-assistant model. So a security bug here can mean poisoned training data,
bypassed abuse protections, or players' anonymous IDs being exposed. Reports
are taken seriously and handled privately.

## Supported Versions

Only `main` receives security fixes. Please check you can still reproduce the
issue on the latest `main` before reporting.

## Reporting a Vulnerability

**Please do not open a public issue, discussion, or pull request for security
problems.**

Report privately through GitHub:
[**Report a vulnerability**](https://github.com/jumpinjet22/Doorman-the-training-game/security/advisories/new)
(Security tab → *Report a vulnerability*).

Helpful things to include:

- what the issue is and what an attacker could do with it
- steps to reproduce, or a proof of concept
- the commit you tested and how it was deployed (local, behind a proxy, which
  `DORMAN_*` settings)
- whether it needs a Cloudflare Turnstile pass, an existing player cookie, or
  neither

Never include real players' cookies, IDs or other personal data. Use a local
instance and test accounts.

This is maintained on a best-effort basis. You can expect an acknowledgement
once the report has been read, updates as it's investigated, and credit in
the advisory once a fix ships (unless you'd rather stay anonymous). Please
give a reasonable window to release a fix before disclosing publicly.

## Scope

In scope, for example:

- ways to cast votes without passing Turnstile, or to get around the per-player
  and per-network rate limits
- forging, tampering with or stealing the signed player cookie, or acting as
  another player
- learning which cards are gold (known-answer) cards, or reading answers or
  scores before the game says they may be shown
- poisoning or skewing the scorer's verdicts: brigading, fake accounts that
  earn trust, or forged timing data
- anything that reveals a player's IP address or a raw network address (only a
  salted hash is meant to be kept, and only for a day)
- cross-site scripting or injection through batch text, scenes, handles or
  explanations; CSRF or `Origin` check bypasses; weakened security headers
- SQL injection, path traversal, or remote code execution in the API, the
  ingest tool or the scorer
- exposure of secrets such as `DORMAN_COOKIE_SECRET`

Out of scope:

- vulnerabilities in third-party services this talks to (Cloudflare Turnstile,
  Postgres)
- attacks that already need access to the host, the database or the secrets
- deliberately insecure configuration, such as running with Cloudflare's
  Turnstile test keys or `DORMAN_SECURE_COOKIES=false` in production, or
  trusting `DORMAN_CLIENT_IP_HEADER` without a proxy that overwrites it
- volumetric denial of service that needs only raw traffic (put a CDN in front)
- findings that only apply to the synthetic example data in `fixtures/`
