---
kind: graph
title: Payments graph
aliases: [payments, 支付图谱, transaction graph, 交易关系]
summary: Merchants and their sub-merchants, the transactions charged for them, the chargebacks disputing those transactions, the settlement batches they are paid out in, and the risk rules that flagged them.
schema: payments
nodes:
  merchant:    { en: "A merchant account", zh: "商户", aliases: [merchants, seller account, 商户, 商家], key: merchant_id }
  transaction: { en: "A card payment", zh: "一笔交易", aliases: [transactions, payment, charge, 交易, 支付], key: txn_id }
  chargeback:  { en: "A cardholder dispute", zh: "拒付", aliases: [chargebacks, dispute, 拒付, 争议, 退单], key: cb_id }
  settlement:  { en: "A settlement batch paid out to merchants", zh: "结算批次", aliases: [settlements, payout, batch, 结算, 打款], key: batch_id }
  risk_rule:   { en: "A fraud rule", zh: "风控规则", aliases: [risk rules, fraud rule, 风控规则, 规则], key: rule_id }
relations:
  charged_by:
    from: transaction
    to: merchant
    en: "The merchant a transaction was charged for"
    zh: "交易所属商户"
    aliases: [merchant of, paid to, charged for, 哪个商户的交易, 收款商户]
  disputes:
    from: chargeback
    to: transaction
    en: "The transaction a chargeback disputes"
    zh: "拒付针对的交易"
    aliases: [disputed transaction, chargeback on, 拒付的是哪笔, 被拒付]
  settled_in:
    from: transaction
    to: settlement
    en: "The settlement batch a transaction was paid out in"
    zh: "交易所在结算批次"
    aliases: [paid out in, settlement of, 哪个批次结算, 结算批次]
  flagged_by:
    from: transaction
    to: risk_rule
    en: "A risk rule that flagged the transaction"
    zh: "命中该交易的风控规则"
    aliases: [triggered, hit rule, caught by, 命中规则, 触发]
  sub_merchant_of:
    from: merchant
    to: merchant
    en: "The parent merchant of a sub-merchant"
    zh: "子商户所属的主商户"
    aliases: [parent merchant, under, platform of, 子商户, 上级商户, 所属平台]
updated: 2026-09-22
---

# Payments graph

Each transaction is charged for one merchant (`charged_by`), may be flagged by risk rules
(`flagged_by`) and is paid out in a settlement batch (`settled_in`). A chargeback disputes one
transaction (`disputes`). Marketplace merchants have sub-merchants (`sub_merchant_of`).
