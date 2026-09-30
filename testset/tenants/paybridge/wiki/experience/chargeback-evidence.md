---
kind: experience
title: Building evidence packs that win chargebacks
aliases: [chargeback evidence, dispute evidence, 拒付举证, 拒付申诉, win a dispute, representment]
keywords: [delivery proof, 3DS, AVS, reason code, compelling evidence, 签收证明, 申诉材料]
summary: "Match the evidence to the reason code: delivery proof and signed receipts for 'not received', 3DS authentication for 'fraud', and the refund policy the customer accepted for 'not as described'."
about: [payments:chargeback, expertise/chargeback-reason-codes]
sources: [2026-08-29-task-7120.json, 2026-09-16-task-7390.json]
updated: 2026-09-16
---

# Building evidence packs that win chargebacks

## Steps that work

1. Read the reason code first; each code has its own evidence list.
2. "Item not received": carrier tracking with delivery confirmation to the billing or shipping
   address, plus a signed receipt for goods over 200 USD.
3. "Fraud / no authorization": the 3DS authentication result and liability shift; AVS and CVV
   matches; previous undisputed payments from the same card and device.
4. "Not as described": the product page at the time of purchase, the refund policy the customer
   accepted, and any messages with the customer.
5. Submit at least 5 days before the deadline; late packs are auto-lost.

## Pitfalls

- Sending the same generic pack for every reason code. Win rate 18% versus 61% when matched.
  Seen many times.
- Screenshots without dates or order ids are ignored by issuers. Seen 6 times.
