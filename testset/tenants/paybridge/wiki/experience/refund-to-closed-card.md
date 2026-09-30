---
kind: experience
title: Refunding to a closed or expired card
aliases: [refund to closed card, card closed refund, 退款到已注销的卡, expired card refund, 退款失败]
keywords: [closed account, card replaced, issuer credit, bank transfer refund, 卡已注销, 换卡]
summary: Refunds to a closed card usually still reach the customer because the issuer credits the new card or the account; wait 10 days before offering a bank transfer refund.
about: [payments:transaction]
sources: [2026-09-12-task-7331.json]
updated: 2026-09-12
---

# Refunding to a closed or expired card

## Steps that work

1. Process the refund to the original card anyway; networks route it to the replacement card or
   the underlying account in most cases.
2. Give the customer the refund's ARN (acquirer reference number) so their bank can trace it.
3. If the issuer returns the refund, or 10 business days pass without it arriving, offer a bank
   transfer refund after verifying the customer's identity.

## Pitfalls

- Sending a bank transfer immediately and the card refund also arriving: a double refund.
  Seen 4 times.
- Not giving the ARN; the customer's bank can't find the money. Seen 3 times.
