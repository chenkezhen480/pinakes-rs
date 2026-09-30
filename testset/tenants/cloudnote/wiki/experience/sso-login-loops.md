---
kind: experience
title: Fixing SSO login loops
aliases: [SSO login loop, SAML redirect loop, SSO 登录循环, 单点登录失败, stuck on login page]
keywords: [SAML, redirect loop, clock skew, ACS URL, Okta, Azure AD, 单点登录, 登录死循环]
summary: Most SAML redirect loops come from a wrong ACS URL or clock skew over 3 minutes on the identity provider; check both before touching certificates.
about: [accounts:workspace, expertise/sso-setup]
sources: [2026-08-14-task-2201.json, 2026-09-09-task-2710.json]
updated: 2026-09-09
---

# Fixing SSO login loops

## Steps that work

1. Ask the admin for the exact IdP (Okta, Azure AD, Google Workspace) and a HAR file of the loop.
2. Compare the ACS URL configured in the IdP with the one on the workspace's SSO settings page.
   A trailing slash or the old `app.` domain is the usual culprit.
3. Check the SAML response timestamps. More than 3 minutes of clock skew makes us reject the
   assertion and restart the login.
4. Only if both are right, check the signing certificate's expiry date.
5. While fixing, give the admin the break-glass login link so they aren't locked out.

## Pitfalls

- Asking the customer to rotate certificates first wastes a day and rarely helps. Seen 5 times.
- Members invited before SSO was enforced keep password logins until an admin removes them.
  Seen 3 times.
