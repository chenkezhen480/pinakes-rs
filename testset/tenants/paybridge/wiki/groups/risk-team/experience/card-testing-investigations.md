---
kind: experience
title: Investigating card-testing attacks
aliases: [card testing attack, card testing investigation, 盗卡测试调查, carding]
keywords: [small test charges, BIN clustering, linked merchants, sub-merchant, 小额试探, 关联商户]
summary: Card-testing shows up as bursts of tiny charges from many cards in a few BIN ranges; cluster by BIN and device, check linked sub-merchants, and block ranges before refunding anything.
about: [payments:risk_rule, expertise/risk-scoring]
sources: [2026-09-23-task-7501.json]
updated: 2026-09-23
---

# Investigating card-testing attacks

## Steps that work

1. Pull the last 24 hours of charges under 2 USD for the merchant and group them by BIN range
   and device fingerprint.
2. Check sub-merchants and merchants sharing a bank account or director; attackers spread tests
   across linked accounts.
3. Block the BIN ranges and devices involved before refunding, or the attack continues on the
   refunded cards.
4. Record the case in the investigations space so the links are kept.

## Pitfalls

- Refunding test charges first; the attackers learn the cards are live. Seen 2 times.
- Looking at one merchant only when the tests were spread across a sub-merchant. Seen 1 time.
