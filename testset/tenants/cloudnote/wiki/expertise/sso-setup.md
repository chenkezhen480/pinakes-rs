---
kind: expertise
title: SSO setup (SAML and OIDC)
aliases: [SSO setup, single sign-on, 单点登录配置, SAML, OIDC, SCIM]
keywords: [Okta, Azure AD, Google Workspace, ACS URL, entity ID, SCIM provisioning, enforce SSO, 强制单点登录]
summary: SSO is included in Enterprise; admins configure SAML 2.0 or OIDC with the ACS URL and entity ID from the workspace settings, and can enforce SSO and provision members with SCIM.
about: [accounts:plan/enterprise]
sources: [admin-guide-v12.pdf]
updated: 2026-07-20
---

# SSO setup (SAML and OIDC)

## Availability

SSO is included in the Enterprise plan. It is not available on Team or Business.

## Configuration

1. In Settings → Security, choose SAML 2.0 or OIDC.
2. Copy the ACS URL (`https://auth.cloudnote.app/saml/<ws_id>/acs`) and entity ID into the
   identity provider.
3. Upload the IdP metadata XML or paste its SSO URL and signing certificate.
4. Test with one admin before enforcing.

## Enforcement and provisioning

- "Enforce SSO" removes password logins for everyone except the break-glass admin.
- SCIM 2.0 provisioning creates and removes members automatically from Okta and Azure AD.
- Clock skew tolerance is 3 minutes.
