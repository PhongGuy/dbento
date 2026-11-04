-- Synthetic Beekeeper Studio app.db from before workspaces, folders and bastions. Every value is fake.
CREATE TABLE saved_connection (
  id integer PRIMARY KEY AUTOINCREMENT NOT NULL,
  createdAt datetime NOT NULL DEFAULT (datetime('now')), updatedAt datetime NOT NULL DEFAULT (datetime('now')),
  version integer NOT NULL DEFAULT 1, connectionType varchar NOT NULL,
  host varchar, port integer, username varchar, defaultDatabase varchar, path varchar, uri varchar,
  uniqueHash varchar(500) NOT NULL DEFAULT 'DEPRECATED', name varchar NOT NULL, password varchar,
  rememberPassword boolean NOT NULL DEFAULT 1, sshEnabled boolean NOT NULL DEFAULT 0,
  sshHost varchar, sshPort integer, sshUsername varchar, sshMode varchar(8) NOT NULL DEFAULT 'keyfile',
  sshPassword varchar, sshKeyfilePassword varchar, ssl boolean NOT NULL DEFAULT 0,
  labelColor varchar(255) DEFAULT 'default', domain varchar(255),
  sslCaFile varchar, sslCertFile varchar, sslKeyFile varchar, sslRejectUnauthorized boolean NOT NULL DEFAULT 1);
INSERT INTO saved_connection (id, name, connectionType, host, port, username, defaultDatabase, password) VALUES
  (1, 'Legacy PG', 'postgresql', 'legacy.example', 5432, 'legacy', 'legacy', '65d645bddde22fbfb20a35e743a2d87118474be84862e4e82c9c6a14b13d57fe21212121212121212121212121212121nEkT8Sf6WPHLPDjyMDC1vSR8qBIEu0kbr7ktLjQxnGc=');
INSERT INTO saved_connection (id, name, connectionType, defaultDatabase) VALUES
  (2, 'Legacy lite', 'sqlite', '/nonexistent/dbento-fixture/legacy.db');
CREATE TABLE favorite_query (
  id integer PRIMARY KEY AUTOINCREMENT NOT NULL, title varchar NOT NULL, text text NOT NULL,
  "database" varchar, connectionHash varchar,
  createdAt datetime NOT NULL DEFAULT (datetime('now')), updatedAt datetime NOT NULL DEFAULT (datetime('now')),
  version integer NOT NULL DEFAULT 1);
INSERT INTO favorite_query (id, title, text) VALUES (1, 'Old query', 'select 1;');
