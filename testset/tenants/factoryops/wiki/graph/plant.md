---
kind: graph
title: Plant graph
aliases: [plant, 工厂设备图谱, equipment graph, 设备关系]
summary: Machines and the production lines they belong to, the spare parts they use, the suppliers of those parts, the work orders repairing them, and how lines feed each other.
space: plant
nodes:
  machine:         { en: "A machine or robot", zh: "设备", aliases: [machines, equipment, asset, 设备, 机器, 机台], key: asset_no }
  production_line: { en: "A production line", zh: "产线", aliases: [lines, line, 产线, 生产线], key: line_id }
  part:            { en: "A spare part", zh: "备件", aliases: [parts, spare part, 备件, 零件, 配件], key: part_no }
  supplier:        { en: "A parts supplier", zh: "供应商", aliases: [suppliers, vendor, 供应商, 厂家], key: supplier_id }
  work_order:      { en: "A maintenance work order", zh: "维修工单", aliases: [work orders, WO, ticket, 工单, 维修单], key: wo_no }
relations:
  part_of:
    from: machine
    to: production_line
    en: "The line a machine belongs to"
    zh: "设备所在产线"
    aliases: [on line, belongs to, which line, 在哪条线, 属于哪条产线]
  uses_part:
    from: machine
    to: part
    en: "A spare part the machine uses"
    zh: "设备使用的备件"
    aliases: [uses, needs, fitted with, 用什么备件, 用到, 装的]
  supplied_by:
    from: part
    to: supplier
    en: "The supplier of a part"
    zh: "备件的供应商"
    aliases: [supplier of, bought from, made by, 谁供货, 哪家供应, 采购自]
  repairs:
    from: work_order
    to: machine
    en: "The machine a work order repairs"
    zh: "工单维修的设备"
    aliases: [work order for, fixing, 修的是, 维修对象]
  feeds:
    from: production_line
    to: production_line
    en: "The downstream line a line feeds"
    zh: "下游产线"
    aliases: [upstream of, supplies, downstream, 上游, 下游, 供给]
updated: 2026-09-19
---

# Plant graph

Machines sit on production lines (`part_of`) and use spare parts (`uses_part`), each bought from
a supplier (`supplied_by`). Work orders repair machines (`repairs`). Lines feed downstream lines
(`feeds`), so a stop upstream starves the lines below it.
