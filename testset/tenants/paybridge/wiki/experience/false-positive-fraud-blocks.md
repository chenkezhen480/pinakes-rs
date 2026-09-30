---
kind: experience
title: Unblocking legitimate customers caught by fraud rules
aliases: [false positive, legit payment blocked, 误拦截, 正常交易被拦, fraud block appeal]
keywords: [false positive, allowlist, rule tuning, 3DS step-up, 误杀, 白名单, 风控误判]
summary: When a genuine customer is blocked, check which rule fired, allowlist the card for 30 days if the history is clean, and prefer a 3DS step-up over a hard block for that rule.
about: [payments:risk_rule, expertise/risk-scoring]
sources: [2026-09-06-task-7260.json]
updated: 2026-09-06
---

# Unblocking legitimate customers caught by fraud rules

## Steps that work

1. Look up the declined transaction and note every rule that fired and the risk score.
2. Check the customer's history: previous successful payments, chargebacks, device and IP.
3. Clean history: add the card to the merchant's allowlist for 30 days and ask the customer to
   retry.
4. If the same rule keeps blocking good customers, propose switching it from "block" to "3DS
   step-up" for scores in the review band.

## Pitfalls

- Allowlisting by email address instead of card fingerprint; fraudsters reuse emails. Seen 3
  times.
- Lowering a rule's threshold globally to fix one merchant's traffic. Seen 2 times.
