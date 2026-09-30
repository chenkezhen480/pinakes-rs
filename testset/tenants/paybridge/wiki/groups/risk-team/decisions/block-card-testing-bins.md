---
kind: decision
title: Block card-testing BIN ranges at checkout
aliases: [BIN block, card testing BINs, 盗卡测试BIN段, block BIN ranges]
keywords: [card testing, BIN ranges, rr_card_testing, block list, 拦截, 卡段]
summary: The risk team decided to block three BIN ranges seen in the card-testing attack on Shadow Parts for 30 days instead of tightening the global velocity rule; the attack stopped within a day and genuine traffic was unaffected.
about: [investigations:risk_rule/rr_card_testing, expertise/risk-scoring]
sources: [2026-09-24-decision-risk-team-01.json]
updated: 2026-09-24
---

# Block card-testing BIN ranges at checkout

## Decision

Block the three BIN ranges used in the card-testing attack on Shadow Parts for 30 days, rather
than lowering the global velocity threshold.

## Context

The investigation found small test charges from cards in three BIN ranges, all flagged by
`rr_card_testing`. Tightening the velocity rule for everyone would have blocked fleet customers
again.

## Outcome

The attack stopped within a day. No genuine customer complaints about the blocked ranges.

## Date

2026-09-24
