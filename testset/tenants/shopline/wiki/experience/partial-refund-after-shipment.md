---
kind: experience
title: Handling partial refunds after shipment
aliases: [partial refund, 部分退款, refund after shipment, 发货后退款, 已发货退款]
keywords: [partial refund, shipped order, manual ledger entry, 部分退款, 已发货, 手工记账]
summary: Check shipment status first; partial refunds after shipment need a manual ledger entry, and the buyer keeps the goods only when the item value is under 50 yuan.
about: [commerce:order, commerce:shipment, expertise/refund-regulations]
sources: [2026-09-02-task-4402.json, 2026-09-18-task-4812.json]
updated: 2026-09-18
---

# Handling partial refunds after shipment

## Steps that work

1. Open the order and check whether it has a shipment. Before shipment, a partial refund is a
   normal cancellation of the line item and needs no ledger work.
2. After shipment, confirm which items the buyer wants refunded and ask for a photo if the reason
   is damage or a wrong item.
3. Issue the refund from the order page, then add a manual ledger entry with the order id and the
   refunded amount. Finance reconciles these entries weekly.
4. If the refunded item is worth less than 50 yuan, tell the buyer they can keep it; otherwise
   create a return label.

## Pitfalls

- Refunding before checking shipment status creates a double refund when the parcel is later
  rejected at the door. Seen 6 times.
- Forgetting the ledger entry leaves the seller's settlement wrong. Seen 11 times.
- Coupons are refunded proportionally; buyers often expect the full coupon back. Seen 4 times.
