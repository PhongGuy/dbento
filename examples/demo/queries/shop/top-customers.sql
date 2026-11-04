-- Top customers by revenue: a join and an aggregate over the whole shop.
SELECT c.id, c.name, c.country, count(DISTINCT o.id) AS orders,
       round(sum(i.quantity * i.unit_price), 2) AS revenue
FROM customers c
JOIN orders o ON o.customer_id = c.id AND o.status <> 'cancelled'
JOIN order_items i ON i.order_id = o.id
GROUP BY c.id
ORDER BY revenue DESC
LIMIT 25;
