-- An UPDATE without WHERE: the app asks before it touches every row.
UPDATE products SET price = round(price * 1.1, 2);
