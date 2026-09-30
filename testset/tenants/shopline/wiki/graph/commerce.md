---
kind: graph
title: Commerce graph
aliases: [commerce, 交易图谱, orders graph, 订单关系]
summary: Orders, the customers who place them, the products they contain, the sellers behind those products, shipments and coupons.
space: commerce
nodes:
  order:    { en: "A customer order", zh: "客户订单", aliases: [orders, 订单], key: order_id }
  customer: { en: "A buyer account", zh: "买家账户", aliases: [customers, buyer, 客户, 买家], key: customer_id }
  product:  { en: "A product listing (SKU)", zh: "商品（SKU）", aliases: [products, SKU, item, 商品], key: sku }
  seller:   { en: "A third-party seller shop", zh: "第三方商家店铺", aliases: [sellers, shop, merchant, 商家, 店铺], key: seller_id }
  shipment: { en: "A parcel shipment with a tracking number", zh: "包裹运单", aliases: [shipments, parcel, tracking number, 快递, 运单, 物流单号], key: tracking_no }
  coupon:   { en: "A discount coupon", zh: "优惠券", aliases: [coupons, voucher, 优惠券, 券], key: code }
relations:
  placed_by:
    from: order
    to: customer
    via: customer_id
    en: "The customer who placed the order"
    zh: "下单的客户"
    aliases: [ordered by, placed, buyer of, 下单人, 所属客户, 下单]
  contains:
    from: order
    to: product
    via: sku
    en: "A product included in the order"
    zh: "订单包含的商品"
    aliases: [includes, bought, items in, 包含, 买了, 购买的商品]
  sold_by:
    from: product
    to: seller
    via: seller_id
    en: "The seller that lists the product"
    zh: "销售该商品的商家"
    aliases: [sells, listed by, 卖家, 由谁卖, 销售商家]
  shipped_via:
    from: order
    to: shipment
    via: tracking_no
    en: "The shipment that carries the order"
    zh: "订单对应的运单"
    aliases: [tracking, shipped with, 运单号, 物流单号, 快递单号, 发货]
  used_coupon:
    from: order
    to: coupon
    via: code
    en: "A coupon applied to the order"
    zh: "订单使用的优惠券"
    aliases: [applied coupon, redeemed, 用了, 使用优惠券, 用券]
updated: 2026-09-20
---

# Commerce graph

Orders link to one customer (`placed_by`), one or more products (`contains`), at most one
shipment (`shipped_via`) and any coupons applied (`used_coupon`). Each product is listed by one
seller (`sold_by`).
