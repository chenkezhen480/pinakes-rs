---
kind: expertise
title: Risk scoring and fraud rules
aliases: [risk scoring, fraud rules, 风控评分, 风控规则, velocity rule, geo mismatch]
keywords: [risk score 0-100, review band, block threshold, velocity, IP country, 评分阈值, 人工审核]
summary: Every payment gets a risk score from 0 to 100; below 60 it is approved, 60–80 goes to 3DS step-up or manual review, above 80 it is blocked; rules such as velocity and geo mismatch add points.
about: [payments:risk_rule]
sources: [risk-engine-doc-v4.pdf]
updated: 2026-08-10
---

# Risk scoring and fraud rules

## Score bands

| Score | Action |
|---|---|
| 0–59 | Approve |
| 60–80 | 3DS step-up, or manual review if 3DS is unavailable |
| 81–100 | Block |

## Common rules

- **Velocity** (`rr_velocity_5`): more than 5 payments from one card in an hour, +25 points.
- **Geo mismatch** (`rr_geo_mismatch`): card country differs from IP country, +15 points.
- **New device and high amount**: first payment from a device above 500 USD, +20 points.

Merchants can have their own thresholds; changes are reviewed by the risk team and logged.
