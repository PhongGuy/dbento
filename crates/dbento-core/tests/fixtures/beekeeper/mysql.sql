-- Synthetic Beekeeper Studio app.db with MySQL-family connections. Every value is fake.
CREATE TABLE bk_migrations (name varchar PRIMARY KEY NOT NULL, run_at datetime NOT NULL DEFAULT (datetime('now')));
INSERT INTO bk_migrations (name) VALUES ('20200101'), ('20260223_create_connection_folders'), ('20260324_add_bastion_auth');
CREATE TABLE connection_folder (
  id integer PRIMARY KEY AUTOINCREMENT NOT NULL, name varchar(255) NOT NULL, description text,
  expanded boolean NOT NULL DEFAULT 1, parentId integer REFERENCES connection_folder (id) ON DELETE SET NULL,
  createdAt datetime NOT NULL DEFAULT (datetime('now')), updatedAt datetime NOT NULL DEFAULT (datetime('now')),
  version integer NOT NULL DEFAULT 1);
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
INSERT INTO connection_folder (id, name, parentId) VALUES (1, 'Shop', NULL);
INSERT INTO saved_connection (id, name, connectionType, host, port, username, defaultDatabase, password, ssl, sslCaFile, sslRejectUnauthorized, labelColor, connectionFolderId) VALUES
  (1, 'Shop MySQL', 'mysql', 'db.shop.example', 3307, 'shop_app', 'shop', '546c5465a868fd187b201b92c667d6c7d4b86ddca7709e4abf7e6e348ae6b78d01010101010101010101010101010101jbe4qQIYNW68+1qffviXC2+1q7aazJxQ7S+5GETnkRs=', 1, '/nonexistent/dbento-fixture-ca.pem', 1, 'red', 1);
INSERT INTO saved_connection (id, name, connectionType, host, port, username, defaultDatabase, password, readOnlyMode) VALUES
  (2, 'Maria (RO)', 'mariadb', 'localhost', 3306, 'maria', 'inventory', 'caf77a407c70230ee61e8eff4b7f8e0797f39d099f4d480c3ecff29d1f746e7604040404040404040404040404040404JA+jUCujIx9hp7pwSvN8JA==', 1);
INSERT INTO saved_connection (id, name, connectionType, username, defaultDatabase, socketPathEnabled, socketPath) VALUES
  (3, 'Socket', 'mysql', 'app', 'app', 1, '/var/run/mysqld/mysqld.sock');
INSERT INTO saved_connection (id, name, connectionType, host, port, username, defaultDatabase) VALUES
  (4, 'TiDB', 'tidb', 'tidb.example', 4000, 'root', 'test');
INSERT INTO saved_connection (id, name, connectionType, host, username, defaultDatabase, iamAuthOptions) VALUES
  (5, 'IAM', 'mysql', 'iam.example', 'iam_user', 'app', '{"iamAuthenticationEnabled":true,"authType":"iam_cli","awsRegion":"eu-west-1"}');
INSERT INTO saved_connection (id, name, connectionType, host, username, defaultDatabase) VALUES
  (6, 'No user', 'mariadb', 'nouser.example', '', 'app');
