---
kind: expertise
title: PCI DSS scope for merchants
aliases: [PCI DSS, PCI compliance, 支付卡安全标准, SAQ A, card data security]
keywords: [SAQ A, SAQ A-EP, SAQ D, tokenization, hosted fields, 卡数据, 令牌化]
summary: Merchants using our hosted payment page or hosted fields qualify for SAQ A; merchants whose servers touch raw card numbers need SAQ D; tokenization keeps card data out of their systems.
about: [payments:merchant]
sources: [security-guide-2026.pdf]
updated: 2026-04-15
---

# PCI DSS scope for merchants

| Integration | Card data touches merchant servers? | Questionnaire |
|---|---|---|
| Hosted payment page (redirect) | No | SAQ A |
| Hosted fields (iframes) | No | SAQ A |
| JavaScript that posts card data from the merchant's page | Page only | SAQ A-EP |
| Direct API with raw card numbers | Yes | SAQ D |

## Tokenization

Stored cards are replaced by tokens; merchants keep the token for repeat payments and never store
the card number. Tokens are useless outside the merchant's account.
