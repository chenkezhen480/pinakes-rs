---
kind: decision
title: Raised the velocity threshold for Voltride
aliases: [velocity threshold change, Voltride velocity, 频次规则调整, rule tuning]
keywords: [rr_velocity_5, 5 to 8 payments, Voltride, false positives, 阈值, 误拦截]
summary: Raised the velocity rule from 5 to 8 payments per card per hour for Voltride E-bikes, whose fleet customers pay for many bikes in a row; false positives fell by 70% without more fraud.
about: [payments:risk_rule/rr_velocity_5, payments:merchant/mch_902, expertise/risk-scoring]
sources: [2026-09-10-decision-marco-01.json]
updated: 2026-09-10
---

# Raised the velocity threshold for Voltride

## Decision

For Voltride E-bikes (mch_902), raise `rr_velocity_5` from 5 to 8 payments per card per hour.

## Context

Voltride's fleet customers buy several bikes with one company card in quick succession; about a
third of their blocks were these legitimate orders.

## Outcome

False positives fell by 70% in two weeks; fraud losses did not increase.

## Date

2026-09-10
