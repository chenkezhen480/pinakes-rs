---
kind: experience
title: 付款后修改收货地址
aliases: [修改地址, 改地址, change shipping address, address change, 地址填错]
keywords: [付款后改地址, 已发货改地址, 拦截快递, change address after payment, reroute parcel]
summary: 未发货时由商家在后台直接改地址；已发货只能联系快递公司拦截转寄，费用由买家承担。
about: [commerce:order, commerce:shipment]
sources: [2026-08-30-task-3902.json]
updated: 2026-09-01
---

# 付款后修改收货地址

## 有效做法

1. 先看订单状态。未发货：通知商家在后台修改地址，并让买家在订单详情里确认新地址。
2. 已发货未签收：联系快递公司申请拦截转寄，告知买家转寄费用约 10–20 元，由买家承担。
3. 已签收：无法修改，只能按退货流程处理。
4. 修改完成后在订单备注中记录新地址和操作人。

## 常见坑

- 让买家自己取消订单再重下，优惠券不会返还，引起投诉。出现过 5 次。
- 跨省转寄可能导致时效翻倍，要提前告知。出现过 2 次。
