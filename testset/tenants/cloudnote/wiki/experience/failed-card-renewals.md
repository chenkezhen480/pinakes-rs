---
kind: experience
title: Recovering failed card renewals
aliases: [failed renewal, card declined, payment failed, 续费失败, 扣款失败, dunning]
keywords: [card declined, renewal failed, update payment method, invoice unpaid, 信用卡被拒, 更新支付方式]
summary: When a renewal charge fails, email the billing contact the same day with a payment link, and move the customer to invoice billing if the card keeps failing; don't wait for the automatic retries alone.
about: [accounts:subscription, accounts:invoice, expertise/billing-dunning]
sources: [2026-09-01-task-2533.json]
updated: 2026-09-11
---

# Recovering failed card renewals

## Steps that work

1. Check the decline reason on the invoice. "Insufficient funds" and "do not honor" usually mean
   a corporate card limit; "expired card" means the card on file is old.
2. Email the billing contact the same day with the hosted payment link for the unpaid invoice.
   Copy the workspace owner if the billing contact has left the company.
3. If the card fails twice, offer invoice billing (bank transfer, net 30) for annual plans.
4. Note the case on the subscription so the renewal team knows before the grace period ends.

## Pitfalls

- Relying only on automatic retries: by day 14 the workspace goes read-only and the customer
  escalates. Seen 8 times.
- Updating the card on the wrong workspace when the customer has several. Seen 2 times.
