---
kind: expertise
title: Chargeback reason codes
aliases: [chargeback reason codes, dispute codes, 拒付原因码, Visa 10.4, Mastercard 4837]
keywords: [fraud, card not present, 13.1, merchandise not received, response deadline, 响应期限, 原因码]
summary: Visa 10.4 and Mastercard 4837 are card-not-present fraud; Visa 13.1 is merchandise not received; merchants have 20 days (Visa) or 30 days (Mastercard) to respond.
about: [payments:chargeback]
sources: [network-rules-digest-2026.pdf]
updated: 2026-05-30
---

# Chargeback reason codes

| Network | Code | Meaning | Best evidence |
|---|---|---|---|
| Visa | 10.4 | Card-not-present fraud | 3DS result, prior undisputed payments |
| Visa | 13.1 | Merchandise / services not received | Delivery confirmation |
| Visa | 13.3 | Not as described | Product page, policy, messages |
| Mastercard | 4837 | No cardholder authorization | 3DS result, AVS/CVV |
| Mastercard | 4853 | Cardholder dispute (not as described, not received) | Delivery proof, policy |

## Deadlines

- Visa: 20 days from the dispute notification.
- Mastercard: 30 days.

Transactions authenticated with 3DS shift fraud liability to the issuer, so 10.4 and 4837
disputes on them are usually won.
