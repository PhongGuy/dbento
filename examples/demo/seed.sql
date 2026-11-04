-- The demo shop. `make demo` builds shop.sqlite from this file with `cargo xtask demo-db`.
-- Every value derives from the row number (no random()), so the data and screenshots are stable.
BEGIN;

CREATE TABLE customers (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT,
    country TEXT NOT NULL,
    joined_on TEXT NOT NULL,
    vip INTEGER NOT NULL DEFAULT 0,
    notes TEXT,
    avatar BLOB
);

CREATE TABLE products (
    id INTEGER PRIMARY KEY,
    sku TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    category TEXT NOT NULL,
    price REAL NOT NULL,
    stock INTEGER,
    attributes TEXT,
    discontinued_on TEXT
);

CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    customer_id INTEGER NOT NULL REFERENCES customers(id),
    ordered_at TEXT NOT NULL,
    status TEXT NOT NULL,
    shipping REAL,
    coupon TEXT
);

CREATE TABLE order_items (
    order_id INTEGER NOT NULL REFERENCES orders(id),
    product_id INTEGER NOT NULL REFERENCES products(id),
    quantity INTEGER NOT NULL,
    unit_price REAL NOT NULL,
    PRIMARY KEY (order_id, product_id)
);

CREATE TABLE events (
    id INTEGER PRIMARY KEY,
    at TEXT NOT NULL,
    kind TEXT NOT NULL,
    customer_id INTEGER,
    payload TEXT,
    message TEXT,
    raw BLOB
);

-- 500 customers: every first name meets every family name once, across several scripts.
WITH RECURSIVE
    n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 500),
    given(k, v) AS (VALUES
        (0, 'Ada'), (1, 'Bjørn'), (2, 'Zoë'), (3, 'José'), (4, 'Ингрид'),
        (5, 'Amélie'), (6, 'Søren'), (7, 'Kaito'), (8, 'Olúwaseun'), (9, 'Mateusz'),
        (10, 'Fatima'), (11, 'Linnea'), (12, 'Priya'), (13, 'Chloé'), (14, 'Dmitri'),
        (15, '愛子'), (16, 'Lars'), (17, '美玲'), (18, 'Ana 🌸'), (19, 'Noah')),
    family(k, v) AS (VALUES
        (0, 'Lovelace'), (1, 'Hansen'), (2, 'Müller'), (3, 'García'), (4, 'Петрова'),
        (5, '山田'), (6, '李'), (7, 'Nguyễn'), (8, 'O''Brien'), (9, 'Kowalski'),
        (10, 'Andersson'), (11, 'Dubois'), (12, 'Rossi'), (13, 'Silva'), (14, 'Tanaka'),
        (15, 'Okafor'), (16, 'Jensen'), (17, 'Novák'), (18, 'Ψαρράς'), (19, 'Smith'),
        (20, 'Cohen'), (21, 'Haddad'), (22, 'Björk'), (23, 'Ivanova'), (24, '김')),
    country(k, v) AS (VALUES
        (0, 'DK'), (1, 'SE'), (2, 'NO'), (3, 'DE'), (4, 'FR'),
        (5, 'JP'), (6, 'CN'), (7, 'BR'), (8, 'US'), (9, 'NG'))
INSERT INTO customers (id, name, email, country, joined_on, vip, notes, avatar)
SELECT
    n.i,
    given.v || ' ' || family.v,
    CASE WHEN n.i % 13 = 0 THEN NULL ELSE 'customer' || n.i || '@example.com' END,
    country.v,
    date('2022-01-01', '+' || (n.i * 3 % 1000) || ' days'),
    n.i % 17 = 0,
    CASE
        WHEN n.i % 50 = 0 THEN 'Long-time customer. ' ||
            replace(hex(zeroblob(12)), '00', 'Prefers delivery after 6 pm and asks for no plastic. ')
        WHEN n.i % 9 = 0 THEN 'Gift wrap'
    END,
    CASE WHEN n.i % 4 = 0 THEN unhex('89504E47' || printf('%08X', n.i * 2654435761 % 4294967296)) END
FROM n
JOIN given ON given.k = n.i % 20
JOIN family ON family.k = (n.i / 20) % 25
JOIN country ON country.k = n.i % 10;

-- 120 products: twelve materials times ten kinds.
WITH RECURSIVE
    n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 120),
    material(k, v) AS (VALUES
        (0, 'Bamboo'), (1, 'Lacquer'), (2, 'Cedar'), (3, 'Ceramic'), (4, 'Copper'),
        (5, 'Linen'), (6, 'Steel'), (7, 'Glass'), (8, 'Walnut'), (9, 'Indigo'),
        (10, 'Matcha'), (11, 'Sakura 桜')),
    kind(k, v, category) AS (VALUES
        (0, 'Bento Box', 'Boxes'), (1, 'Chopsticks', 'Utensils'), (2, 'Tea Bowl', 'Tea'),
        (3, 'Lunch Bag', 'Bags'), (4, 'Rice Paddle', 'Utensils'), (5, 'Furoshiki Cloth', 'Textiles'),
        (6, 'Teapot', 'Tea'), (7, 'Serving Tray', 'Serving'), (8, 'Spoon Set', 'Utensils'),
        (9, 'Spare Lid', 'Parts')),
    colour(k, v) AS (VALUES (0, 'natural'), (1, 'black'), (2, 'red'), (3, 'indigo'), (4, 'white'), (5, 'green'))
INSERT INTO products (id, sku, name, category, price, stock, attributes, discontinued_on)
SELECT
    n.i,
    printf('SKU-%05d', n.i),
    material.v || ' ' || kind.v,
    kind.category,
    round(3.99 + (n.i * 37 % 120) * 0.5, 2),
    CASE WHEN n.i % 15 = 0 THEN NULL ELSE n.i * 53 % 400 END,
    json_object('colour', colour.v, 'weight_g', 50 + n.i * 13 % 900,
                'dishwasher_safe', json(CASE WHEN n.i % 3 = 0 THEN 'true' ELSE 'false' END)),
    CASE WHEN n.i % 15 = 0 THEN date('2025-01-01', '+' || n.i || ' days') END
FROM n
JOIN material ON material.k = n.i % 12
JOIN kind ON kind.k = (n.i / 12) % 10
JOIN colour ON colour.k = n.i % 6;

-- 4,000 orders over twenty months; quadratic residues leave some customers buying a lot
-- and others never.
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 4000)
INSERT INTO orders (id, customer_id, ordered_at, status, shipping, coupon)
SELECT
    i,
    1 + (i * i % 997) % 500,
    datetime('2024-01-01 08:00:00', '+' || (i * 547 % 600) || ' days',
             '+' || (i * 37 % 720) || ' minutes'),
    CASE
        WHEN i % 23 = 0 THEN 'cancelled'
        WHEN i % 7 = 0 THEN 'pending'
        WHEN i % 5 = 0 THEN 'shipped'
        ELSE 'delivered'
    END,
    CASE WHEN i % 4 = 0 THEN NULL ELSE 4.95 END,
    CASE WHEN i % 31 = 0 THEN 'BENTO10' END
FROM n;

-- One to four lines per order, 10,000 in all; products 111 to 120 are never ordered.
WITH RECURSIVE line(k) AS (SELECT 0 UNION ALL SELECT k + 1 FROM line WHERE k < 3)
INSERT INTO order_items (order_id, product_id, quantity, unit_price)
SELECT o.id, p.id, 1 + (o.id + line.k) % 3, p.price
FROM orders o
JOIN line ON line.k <= o.id % 4
JOIN products p ON p.id = 1 + (o.id * 13 + line.k * 29) % 110;

-- 200,000 events, thirteen seconds apart, for scrolling and fetch on scroll.
WITH RECURSIVE
    n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 200000),
    kind(k, v) AS (VALUES
        (0, 'page_view'), (1, 'search'), (2, 'add_to_cart'), (3, 'checkout'),
        (4, 'login'), (5, 'logout'), (6, 'error'))
INSERT INTO events (id, at, kind, customer_id, payload, message, raw)
SELECT
    n.i,
    datetime('2024-06-01 00:00:00', '+' || (n.i * 13) || ' seconds'),
    kind.v,
    CASE WHEN n.i % 6 = 0 THEN NULL ELSE 1 + n.i % 500 END,
    json_object('path', '/p/' || (1 + n.i % 120), 'ms', n.i * 7 % 900),
    CASE WHEN kind.k = 6 THEN 'upstream timeout after ' || (n.i % 30) || ' s on /p/' || (1 + n.i % 120) END,
    CASE WHEN n.i % 10 = 0 THEN unhex(printf('%08X', n.i)) END
FROM n
JOIN kind ON kind.k = n.i % 7;

CREATE INDEX orders_customer ON orders(customer_id);
CREATE INDEX order_items_product ON order_items(product_id);
CREATE INDEX events_at ON events(at);

COMMIT;
