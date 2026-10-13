# Security policy

## Reporting a vulnerability

Please **don't open a public issue** for a security problem. Report it privately through
GitHub: on this repository, **Security → Report a vulnerability** (GitHub's private
vulnerability reporting). Only the maintainers see the report.

Say what is affected (version or commit, configuration), how to reproduce it, and what an
attacker gains. We aim to acknowledge a report within 3 days and to agree on a fix and a
disclosure date with you within 14 days; we credit reporters who want to be named.

## Supported versions

Pinakes is pre-1.0. Fixes go into the latest release and the `main` branch only.

## What is in scope

Pinakes holds several tenants' knowledge in one service, calls an LLM with content from users
and the web, and fetches pages from sites a tenant trusts. Reports about these are especially
welcome:

- **Isolation**: reading or inferring another tenant's, group's or user's pages, records, graph
  spaces or search terms; widening a `Scope` from a request body.
- **Authentication**: API key or JWT handling, the admin key, the MCP endpoint.
- **Prompt injection** that gets fetched or uploaded content past the librarian's screening and
  verification into the wiki, or makes it act on instructions.
- **Server-side request forgery**: acquisition reaching hosts off a tenant's allowlist or on
  private networks (note that behind an HTTP proxy, the proxy resolves names; see
  docs/guide.md "Lint, review and filling gaps").
- **Untrusted files**: uploads that break out of the converter, exhaust memory or time
  (zip bombs, huge XML), or escape a tenant's folder through names.
- **Deletion**: data of a deleted user, group or tenant that survives in files, the stores, the
  indexes or the vectors.

Out of scope: the test-only key pairs in `tests/fixtures/jwt/` (published on purpose for the
tests), denial of service by a caller holding a valid key on an unbounded deployment, and
findings that need the admin key or access to the host.

## Deployment advice

Keep the service bound to `127.0.0.1` (the default) or behind a TLS-terminating proxy; keep
`PINAKES_ADMIN_KEY` and `model.conf` out of images and repositories; give API keys only to
trusted backends (use JWT mode when calls come from end users' machines).
