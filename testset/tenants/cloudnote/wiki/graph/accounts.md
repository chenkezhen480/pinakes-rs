---
kind: graph
title: Accounts graph
aliases: [accounts, 账户图谱, customer accounts, 客户账户关系]
summary: Customer workspaces, their subscriptions and plans, invoices, members and connected integrations.
schema: accounts
nodes:
  workspace:    { en: "A customer workspace (one company)", zh: "客户工作区（一家公司）", aliases: [workspaces, customer, company, 工作区, 客户], key: ws_id }
  subscription: { en: "A paid subscription", zh: "付费订阅", aliases: [subscriptions, contract, 订阅, 合同], key: sub_id }
  plan:         { en: "A pricing plan", zh: "套餐", aliases: [plans, tier, pricing plan, 套餐, 版本], key: plan_code }
  invoice:      { en: "An invoice", zh: "账单发票", aliases: [invoices, bill, 账单, 发票], key: invoice_no }
  member:       { en: "A person in a workspace", zh: "工作区成员", aliases: [members, user, seat, 成员, 用户], key: member_id }
  integration:  { en: "A connected third-party app", zh: "第三方集成", aliases: [integrations, app, connector, 集成, 插件], key: integration_id }
relations:
  has_subscription:
    from: workspace
    to: subscription
    en: "The workspace's subscription"
    zh: "工作区的订阅"
    aliases: [subscribed, subscription of, 订阅了, 的订阅]
  on_plan:
    from: subscription
    to: plan
    en: "The plan a subscription is on"
    zh: "订阅所属套餐"
    aliases: [which plan, plan of, tier of, 什么套餐, 哪个版本]
  billed_to:
    from: invoice
    to: workspace
    en: "The workspace an invoice is billed to"
    zh: "账单所属工作区"
    aliases: [invoiced to, bill for, 开给, 账单属于]
  member_of:
    from: member
    to: workspace
    en: "A workspace the member belongs to"
    zh: "成员所在工作区"
    aliases: [belongs to, works at, seat in, 属于, 在哪个工作区]
  connected_to:
    from: workspace
    to: integration
    en: "An integration the workspace has connected"
    zh: "工作区连接的集成"
    aliases: [connected, uses, integrated with, 接入了, 连接了, 集成了]
updated: 2026-09-15
---

# Accounts graph

A workspace has one active subscription (`has_subscription`), which is on one plan (`on_plan`).
Invoices are billed to a workspace (`billed_to`). Members can belong to several workspaces
(`member_of`), and workspaces connect integrations (`connected_to`).
