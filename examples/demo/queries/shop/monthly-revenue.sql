-- Revenue per month: dates are ISO text, so strftime groups them.
SELECT strftime('%Y-%m', o.ordered_at) AS month, count(DISTINCT o.id) AS orders,
       round(sum(i.quantity * i.unit_price), 2) AS revenue
FROM orders o
JOIN order_items i ON i.order_id = o.id
WHERE o.status <> 'cancelled'
GROUP BY month
ORDER BY month;
