-- Synthetic Beekeeper Studio app.db (2026 schema). Every value is fake.
CREATE TABLE bk_migrations (name varchar PRIMARY KEY NOT NULL, run_at datetime NOT NULL DEFAULT (datetime('now')));
INSERT INTO bk_migrations (name) VALUES ('20200101'), ('20260223_create_connection_folders'), ('20260324_add_bastion_auth');
CREATE TABLE connection_folder (
  id integer PRIMARY KEY AUTOINCREMENT NOT NULL, name varchar(255) NOT NULL, description text,
  expanded boolean NOT NULL DEFAULT 1, parentId integer REFERENCES connection_folder (id) ON DELETE SET NULL,
  createdAt datetime NOT NULL DEFAULT (datetime('now')), updatedAt datetime NOT NULL DEFAULT (datetime('now')),
  version integer NOT NULL DEFAULT 1);
INSERT INTO connection_folder (id, name, parentId) VALUES (1, 'Prod', NULL), (2, 'EU', 1);
CREATE TABLE saved_connection (
  id integer PRIMARY KEY AUTOINCREMENT NOT NULL,
  createdAt datetime NOT NULL DEFAULT (datetime('now')), updatedAt datetime NOT NULL DEFAULT (datetime('now')),
  version integer NOT NULL DEFAULT 1, connectionType varchar NOT NULL,
  host varchar, port integer, username varchar, defaultDatabase varchar, path varchar, url varchar,
  uniqueHash varchar(500) NOT NULL DEFAULT 'DEPRECATED', name varchar NOT NULL, password varchar,
  rememberPassword boolean NOT NULL DEFAULT 1, sshEnabled boolean NOT NULL DEFAULT 0,
  sshHost varchar, sshPort integer, sshUsername varchar, sshMode varchar(8) NOT NULL DEFAULT 'keyfile',
  sshPassword varchar, sshKeyfilePassword varchar, ssl boolean NOT NULL DEFAULT 0,
  labelColor varchar(255) DEFAULT 'default', sslCaFile varchar, sslCertFile varchar, sslKeyFile varchar,
  sslRejectUnauthorized boolean NOT NULL DEFAULT 1, workspaceId integer NOT NULL DEFAULT -1,
  socketPathEnabled boolean NOT NULL DEFAULT 0, socketPath varchar(255), options text NOT NULL DEFAULT '{}',
  readOnlyMode boolean NOT NULL DEFAULT 0, azureAuthOptions text NOT NULL DEFAULT '{}', authId integer,
  iamAuthOptions text NOT NULL DEFAULT '{}',
  connectionFolderId integer REFERENCES connection_folder (id) ON DELETE SET NULL,
  position float NOT NULL DEFAULT 0, sshBastionHost varchar(255), sshBastionHostPort integer,
  sshBastionMode varchar(8) NOT NULL DEFAULT 'agent', sshBastionUsername varchar, sshBastionKeyfile varchar,
  sshBastionPassword varchar, sshBastionKeyfilePassword varchar);
INSERT INTO saved_connection (id, name, connectionType, host, port, username, defaultDatabase, password, ssl, sslCaFile, sslRejectUnauthorized, labelColor, connectionFolderId) VALUES
  (1, 'Orders DB', 'postgresql', 'db.orders.example', 5433, 'orders_app', 'orders', '546c5465a868fd187b201b92c667d6c7d4b86ddca7709e4abf7e6e348ae6b78d01010101010101010101010101010101jbe4qQIYNW68+1qffviXC2+1q7aazJxQ7S+5GETnkRs=', 1, '/nonexistent/dbento-fixture-ca.pem', 1, 'red', 2);
INSERT INTO saved_connection (id, name, connectionType, host, username, defaultDatabase, password, ssl, readOnlyMode) VALUES
  (2, 'Reporting (RO)', 'psql', 'localhost', 'reporter', 'reports', '6e58e25413ec33f23fab65bf19a0cdae0919a899b0040463130caebaa6f9eca102020202020202020202020202020202/ELOx60NvLtbs5NNL9G96WMGHEsSGXAjMXmajK8t02o=', 1, 1);
INSERT INTO saved_connection (id, name, connectionType, username, defaultDatabase, ssl, rememberPassword, socketPathEnabled) VALUES
  (3, 'Local socket', 'postgres', 'app', 'app', 1, 0, 1);
INSERT INTO saved_connection (id, name, connectionType, host, username, defaultDatabase, password, ssl, sslCertFile, sslKeyFile, sslRejectUnauthorized) VALUES
  (4, 'Client certs', 'postgresql', 'db.certs.example', 'certs', 'certs', 'caf77a407c70230ee61e8eff4b7f8e0797f39d099f4d480c3ecff29d1f746e7604040404040404040404040404040404JA+jUCujIx9hp7pwSvN8JA==', 1, '/nonexistent/dbento-fixture-client.pem', '/nonexistent/dbento-fixture-client.key', 0);
INSERT INTO saved_connection (id, name, connectionType, host, username, defaultDatabase, password, iamAuthOptions) VALUES
  (5, 'IAM', 'postgresql', 'iam.example', 'iam_user', 'app', '9022847af0ebeba5e787bcc6c1e9bd497a57d1029bc6ed0bd1544689a939823805050505050505050505050505050505tJh81lEnnpBUWJIREppzKA==', '{"iamAuthenticationEnabled":true,"authType":"iam_cli","awsRegion":"eu-west-1"}');
INSERT INTO saved_connection (id, name, connectionType, defaultDatabase) VALUES
  (6, 'Shop lite', 'sqlite', 'lite/shop.db');
INSERT INTO saved_connection (id, name, connectionType, defaultDatabase, readOnlyMode) VALUES
  (7, 'Abs lite', 'sqlite', '/nonexistent/dbento-fixture/abs.db', 1);
INSERT INTO saved_connection (id, name, connectionType, host, username, defaultDatabase) VALUES
  (8, 'Cockroach', 'cockroachdb', 'crdb.example', 'root', 'defaultdb');
INSERT INTO saved_connection (id, name, connectionType, host, username, defaultDatabase, password, sshEnabled, sshHost, sshUsername, sshMode, sshPassword) VALUES
  (9, 'Tunnelled', 'postgresql', '10.0.0.5', 'app', 'app', '71d966b9497afd985f4dc4aa2b6243d1c695406924668dac599531791f7c064509090909090909090909090909090909lrAL2YwKJkD00M23bwFDkC7fkH1jFwyzM6lj4By/ePM=', 1, 'jump.example', 'jumper', 'userpass', '32b66ff2ed164478caf9f5f5eabd84ec9e1a63abb569bfab2d8f4024ab742b6199999999999999999999999999999999R5YObrpjFhsXb7PL5pc44Q==');
INSERT INTO saved_connection (id, name, connectionType, host, username, defaultDatabase, workspaceId) VALUES
  (10, 'Cloud', 'postgresql', 'cloud.example', 'cloud', 'app', 7);
INSERT INTO saved_connection (id, name, connectionType, host, username, defaultDatabase, password) VALUES
  (11, 'Broken secret', 'postgresql', 'broken.example', 'broken', 'broken', '6df738827bbbe68919c08df4e5453505f5d158b4e5e3488ad118848a51025d860b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0bYWc3XV6x6C1eLq4hH5GKrcJKL9ajG4wLEf/RNfYe6cs=');
INSERT INTO saved_connection (id, name, connectionType, host, username, defaultDatabase) VALUES
  (12, 'No user', 'postgresql', 'nouser.example', '', 'app');
INSERT INTO saved_connection (id, name, connectionType, host, username, defaultDatabase, password, connectionFolderId) VALUES
  (13, 'Orders DB', 'postgresql', 'db2.orders.example', 'orders_ro', 'orders', '644d58da1d1d538d9e5c1acddab33287fee9c1f95921a7aef2de173c91f762270d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0dvfx/o5DFSXYw5iqSW4vD4axp0DHwVX0g9GDEmFmYP6M=', 1);
CREATE TABLE query_folder (
  id integer PRIMARY KEY AUTOINCREMENT NOT NULL, name varchar(255) NOT NULL, description text,
  expanded boolean NOT NULL DEFAULT 1, parentId integer REFERENCES query_folder (id) ON DELETE SET NULL,
  createdAt datetime NOT NULL DEFAULT (datetime('now')), updatedAt datetime NOT NULL DEFAULT (datetime('now')),
  version integer NOT NULL DEFAULT 1);
INSERT INTO query_folder (id, name, parentId) VALUES (1, 'Reports', NULL), (2, 'Monthly / Q?', 1);
CREATE TABLE favorite_query (
  id integer PRIMARY KEY AUTOINCREMENT NOT NULL, title varchar NOT NULL, text text NOT NULL, excerpt text,
  "database" varchar NOT NULL DEFAULT '[blank]', connectionHash varchar NOT NULL DEFAULT 'DEPRECATED',
  queryFolderId integer REFERENCES query_folder (id) ON DELETE SET NULL, position float NOT NULL DEFAULT 0,
  createdAt datetime NOT NULL DEFAULT (datetime('now')), updatedAt datetime NOT NULL DEFAULT (datetime('now')),
  version integer NOT NULL DEFAULT 1);
INSERT INTO favorite_query (id, title, text, queryFolderId) VALUES
  (1, 'Top orders', 'select * from orders order by total desc limit 10;', 1),
  (2, 'Top orders', 'select * from orders order by total desc limit 10;', 1),
  (3, 'Top orders', 'select count(*) from orders;', 1),
  (4, 'a/b: c?', 'select 3;', 2),
  (5, 'Blank', '   ', NULL),
  (6, 'CON', 'select 4;', NULL),
  (7, '..', 'select 5;', NULL);
