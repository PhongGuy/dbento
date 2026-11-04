-- shop-prod is read-only, so the database itself refuses this write.
UPDATE customers SET vip = 1 WHERE id = 1;
