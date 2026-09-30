---
kind: graph
title: Logistics network graph
aliases: [network, 物流网络, shipment network, 运输网络]
summary: Shipments and where they are stored, who carries them, which route they follow and which customs declaration covers them; routes link origin and destination warehouses.
schema: network
nodes:
  shipment:            { en: "A shipment under one air waybill or bill of lading", zh: "一票货（运单）", aliases: [shipments, AWB, waybill, 货物, 运单, 一票货], key: awb }
  warehouse:           { en: "A warehouse or hub", zh: "仓库或枢纽", aliases: [warehouses, hub, depot, 仓库, 仓, 枢纽], key: wh_code }
  carrier:             { en: "A transport company", zh: "承运商", aliases: [carriers, shipping line, airline, 承运商, 船公司], key: carrier_code }
  route:               { en: "A lane between two warehouses", zh: "线路", aliases: [routes, lane, 线路, 航线], key: route_id }
  customs_declaration: { en: "A customs declaration", zh: "报关单", aliases: [declaration, customs entry, 报关单, 报关], key: decl_no }
relations:
  stored_at:
    from: shipment
    to: warehouse
    en: "The warehouse a shipment is currently in"
    zh: "货物当前所在仓库"
    aliases: [where is, located at, sitting in, 在哪个仓, 存放在, 在哪]
  carried_by:
    from: shipment
    to: carrier
    en: "The carrier moving the shipment"
    zh: "承运该货物的承运商"
    aliases: [carrier of, shipped by, moved by, 谁在运, 承运, 哪家船公司]
  follows_route:
    from: shipment
    to: route
    en: "The route a shipment travels on"
    zh: "货物走的线路"
    aliases: [route of, travels on, lane, 走哪条线, 走的线路]
  declared_under:
    from: shipment
    to: customs_declaration
    en: "The customs declaration covering the shipment"
    zh: "货物对应的报关单"
    aliases: [declaration for, customs entry of, 报关单号, 对应报关单]
  departs_from:
    from: route
    to: warehouse
    en: "Where a route starts"
    zh: "线路起点"
    aliases: [origin, starts at, 起点, 从哪出发]
  arrives_at:
    from: route
    to: warehouse
    en: "Where a route ends"
    zh: "线路终点"
    aliases: [destination, ends at, 终点, 到哪]
updated: 2026-09-18
---

# Logistics network graph

A shipment sits in one warehouse at a time (`stored_at`), is moved by a carrier (`carried_by`)
along a route (`follows_route`) and is covered by a customs declaration (`declared_under`).
Routes connect an origin (`departs_from`) and a destination (`arrives_at`) warehouse.
