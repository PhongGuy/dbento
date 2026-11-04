-- Products nobody has ordered yet, with their JSON attributes and NULL stock.
SELECT p.id, p.sku, p.name, p.category, p.price, p.stock, p.attributes, p.discontinued_on
FROM products p
WHERE NOT EXISTS (SELECT 1 FROM order_items i WHERE i.product_id = p.id)
ORDER BY p.id;
