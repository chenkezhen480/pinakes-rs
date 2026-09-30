---
kind: expertise
title: Billing retries and grace period
aliases: [dunning, billing retries, grace period, 宽限期, 扣款重试, unpaid invoice policy]
keywords: [retry schedule, day 1 3 7, 14-day grace, read-only, suspension, 欠费, 只读]
summary: Failed charges are retried on days 1, 3 and 7; after a 14-day grace period the workspace becomes read-only, and after 60 days unpaid it is suspended.
about: [accounts:subscription, accounts:invoice]
sources: [billing-policy-2026.pdf]
updated: 2026-06-30
---

# Billing retries and grace period

## Retry schedule

When a renewal charge fails, the card is retried automatically on day 1, day 3 and day 7. The
billing contact gets an email after each failure.

## Grace period

- Days 0–14: the workspace works normally, with a banner for admins.
- Day 14: the workspace becomes read-only until the invoice is paid.
- Day 60: the workspace is suspended; data is kept for 90 more days, then deleted.

## Exceptions

Customer success can extend the grace period for annual contracts, for example while a
customer's procurement approves a renewal. Extensions are recorded on the subscription.
