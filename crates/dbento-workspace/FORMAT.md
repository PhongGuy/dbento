# The `dbento.toml` workspace format

A Dbento workspace is a folder holding a `dbento.toml` file and, optionally, a `queries/` folder.
This document describes format version 1 as the `dbento-workspace` crate reads it. The file
holds connection settings and folder colours, never a password, so it can be committed to git.

## Stability

- The format version is the root key `dbento`. This crate reads version 1 only.
- Every error has a stable code (see [Error codes](#error-codes)). A code is never renamed. A new
  kind of error gets a new code.
- The non-error enums of the crate (`Engine`, `Color`, `TlsMode`, `AuthLevel`, `SecretSource`,
  `Databases`, `QueryLanguage` and the other kinds) are closed vocabularies of the format: a new
  variant is a format change and a breaking release. The error enums stay `#[non_exhaustive]`.

## Root keys

```toml
dbento = 1

[folders."Prod/EU"]
color = "orange"

[connections.app-prod]
engine = "postgres"
host = "db.example.com"
database = "app"
user = "readonly"
read_only = true
folder = "Prod/EU"
```

| Key | Type | Meaning |
| --- | --- | --- |
| `dbento` | integer, required | The format version; must be `1`. |
| `connections` | table of tables | One table per connection, keyed by its name. |
| `folders` | table of tables | One table per folder path, holding only `color`. |

Unknown keys are refused at every level: the root, a folder table, a connection table, and a
`secret = { env = "..." }` table.

A connection name matches `[A-Za-z0-9_-]+`.

## Keys every connection takes

| Key | Type | Meaning |
| --- | --- | --- |
| `engine` | string, required | The engine (next section). |
| `read_only` | bool | Every statement runs read-only, enforced by the database. Defaults to `false`. |
| `color` | string | One of `red`, `orange`, `yellow`, `green`, `teal`, `blue`, `purple`, `gray`. Red marks production. |
| `folder` | string | The folder path the connection sits in. |
| `statement_timeout` | string | A duration such as `30s` or `5m`, above zero and at most 2147483647 ms. |

## Keys per engine

`engine` takes `postgres`, `mysql` (alias `mariadb`), `mssql` (alias `sqlserver`), `sqlite`,
`duckdb`, `mongodb` (alias `mongo`), `surrealdb` (alias `surreal`), `redis` (alias `valkey`) or
`dynamodb` (alias `dynamo`). A key that belongs to another engine is refused with `invalid`.

| Engine | Required | Optional |
| --- | --- | --- |
| `sqlite`, `duckdb` | `path` (relative paths resolve against the workspace folder) | none |
| `postgres` | `host`, `database`, `user` | `port` (5432), `secret`, `tls`, `tls_ca`, `tls_cert`, `tls_key`, `settings`, `databases`, `ssh` |
| `mysql` | `host`, `user` | `database`, `port` (3306), `secret`, `tls`, `tls_ca`, `tls_cert`, `tls_key`, `ssh` |
| `mssql` | `host`, `user` | `database`, `port` (1433), `secret`, `tls`, `tls_ca`, `databases`, `read_only_user`, `ssh` |
| `mongodb` | `uri` (`mongodb://` or `mongodb+srv://`) | `secret`, only with a user in `uri` (`"none"` is always accepted) |
| `surrealdb` | `host`, `namespace`, `database` | `port` (8000), `user`, `auth_level` (`root`, `namespace` or `database`, required with `user`), `secret` (only with `user`; `"none"` is always accepted), `tls`, `tls_ca`, `ssh` |
| `redis` | none | `host` (`127.0.0.1`), `port` (6379), `database` (a DB index, `0`), `user`, `secret`, `tls`, `tls_ca`, `tls_cert`, `tls_key`, `ssh` |
| `dynamodb` | none | `region`, `endpoint`, `access_key_id`, `secret` (only with `access_key_id`, and never `"none"` there), `consistent_read` (bool) |

Further rules:

- `tls` is `"verify"` or `"off"`. Postgres, MySQL and Redis default to `off` on `localhost`, a
  loopback address or a unix socket directory (a host starting with `/`), else `verify`. SQL
  Server and SurrealDB default to `verify` on every host. `tls_cert` and `tls_key` go together and
  need `tls = "verify"`.
- `settings` (Postgres only) is a table of strings. A name is an identifier or two identifiers
  joined by one dot. `client_encoding`, `bytea_output`, `statement_timeout` and
  `default_transaction_read_only` are reserved.
- `databases` (Postgres and SQL Server) is `"all"` or a non-empty list of distinct names matching
  `[A-Za-z0-9_-]+`; SQL Server also refuses two names that differ only in case. Each database is a
  child connection named `<parent>/<database>`.
- `read_only_user` (SQL Server only) is the non-blank (not empty or only whitespace) reader user
  that read-only work impersonates.
- `endpoint` (DynamoDB) is an `http` or `https` URL with no user, path, query or fragment. Plain
  `http` is allowed only to `localhost` or a loopback address. `region` and `access_key_id` must
  be non-blank (not empty or only whitespace).
- `ssh` is a sub-table; see [SSH tunnels](#ssh-tunnels).

## SSH tunnels

A server connection (`postgres`, `mysql`, `mssql`, `surrealdb` or `redis`) may be reached through
an SSH tunnel, written as a sub-table directly under its entry:

```toml
[connections.prod.ssh]
host = "bastion"
```

| Key | Meaning | When absent |
| --- | --- | --- |
| `host` (required) | the SSH host or a `Host` alias of `~/.ssh/config` | refused |
| `port` | the SSH port, 1-65535 | `Port` from `~/.ssh/config`, else 22 |
| `user` | the SSH user | `User` from `~/.ssh/config`, else the local user |
| `auth` | `"agent"`, `"key"` or `"password"`: only that way is tried | the agent, then key files, then a stored password |
| `identity_file` | the private key; `~/` is the home folder, a relative path the workspace folder | `IdentityFile` from `~/.ssh/config`, else `~/.ssh/id_ed25519`, `id_ecdsa` and `id_rsa` |
| `keepalive` | whether the tunnel sends keepalives (bool) | `true` |

The connection's own `host` and `port` are then resolved on the SSH host's side. Values written
in `dbento.toml` win over `~/.ssh/config`. A derived child connection uses its parent's tunnel.

Refused with `invalid` when the file is read:

- `ssh` on `mongodb`, `dynamodb`, `sqlite` or `duckdb`;
- `ssh` on a Postgres connection whose `host` is a unix socket directory (starts with `/`);
- an empty or blank `host`, a `port` of 0, or an `auth` word other than the three above;
- `identity_file` together with `auth = "agent"` or `auth = "password"`;
- any other key in the table, and a `password` key, which is refused as a secret.

Refused when the connection is opened, since `~/.ssh/config` is read then, never at load:

- a `Match` block anywhere in `~/.ssh/config` or a file it `Include`s;
- `ProxyJump` or `ProxyCommand` (other than `none`) for the host;
- a `%` token in a value Dbento would use: `HostName`, and `User` or `IdentityFile` when
  `dbento.toml` does not set them.

In each of these cases, write `host`, `port`, `user` and `identity_file` in `dbento.toml` instead.

An `identity_file` written as an absolute path outside the home folder loads with a warning,
since a teammate sharing the workspace will rarely have the key at that path; prefer `~/` or a
`~/.ssh/config` alias.

## Secrets

No secret is ever written in `dbento.toml`.

- A key named `password`, in any letter case, at any depth (including inside arrays and inline
  tables), is refused with `password` and its dotted key, such as `connections.a.settings.password`.
- `secret` says where the secret comes from: `"keychain"` (the default for server engines),
  `"none"`, or `{ env = "NAME" }`. Any other word is refused without quoting it, since it may be
  the password itself.
- A MongoDB `uri` is refused with `secret` (key `uri`) when any `:` appears anywhere before its
  last `@`, since a password may hold `/` and `?`. This also refuses an `@` written after a
  `host:port`, as in `mongodb://h:27017/db?appName=a@b`: write that `@` as `%40`. The uri is
  also refused when it holds the option `tlsCertificateKeyFilePassword`, `proxyPassword`,
  or `authMechanismProperties` holding `AWS_SESSION_TOKEN`. Option keys match in any case.
- A DynamoDB `endpoint` holding a user or password is refused with `secret` (key `endpoint`).
- A MongoDB `uri` port, when written for a host, must be ASCII digits in 1-65535, bracketed IPv6
  hosts and `mongodb+srv://` included; otherwise `invalid`. The message never quotes the uri, so a
  uri such as `mongodb://u:secret/` can never be shown as the connection's target.

No error message quotes the value of `uri`, `endpoint` or `secret`.

An SSH tunnel's password and key passphrase are secrets too, kept in the OS keychain beside the
database secret, or read from `DBENTO_SSH_PASSWORD_<NAME>` and `DBENTO_SSH_KEY_PASSPHRASE_<NAME>`
when the keychain has none (`<NAME>` is the connection name upper-cased, `-` written `_`). The stored SSH password is tied to the resolved `user@host:port`, so a
changed SSH host, port or user does not reuse it.

The full MongoDB driver syntax check is not part of the format: Dbento runs it when it connects
and when it saves a connection, so a malformed but secret-free uri fails that one connection only.

## Folders and colours

A folder path is `/`-separated, for example `Prod/EU`. Every segment is non-empty, has no leading
or trailing whitespace and no control character. Zero-width characters (U+200B, U+FEFF) are
dropped before the path is read. A folder table holds only `color`.

A connection's effective colour is its own `color`, else the colour of the nearest ancestor
folder (its own folder included) that declares one, else none. Folders a connection names but
`[folders]` does not declare still exist in the tree, without a colour of their own.

## The `queries/` folder

Saved queries are files below `queries/` with the extension of their language, in any case:

| Extension | Language |
| --- | --- |
| `.sql` | SQL |
| `.js` | MongoDB shell |
| `.surql` | SurrealQL |
| `.redis` | Redis commands |
| `.partiql` | PartiQL |

- The first folder below `queries/` names the connection the file runs on:
  `queries/app-prod/report.sql` runs on `app-prod`. A file directly in `queries/` binds nothing.
  Under a parent with `databases`, `queries/<parent>/<database>/...` runs on the child
  `<parent>/<database>`, and a file directly in `queries/<parent>/` runs on the parent.
- Every read and write stays inside `queries/`: a path that leaves it, lexically or through a
  symlink, is refused.
- A query file is at most 16 MiB (`MAX_QUERY_FILE_BYTES`, 16777216 bytes) and must be UTF-8.

## Error codes

`OpenError::code()` returns one of these codes:

| Code | Meaning | Names |
| --- | --- | --- |
| `not-found` | No `dbento.toml` in the folder (or its parents, when discovering). | |
| `io` | The file exists but cannot be read. | |
| `parse` | Not valid TOML, a missing or non-integer `dbento`, a key the format does not know, or a value of the wrong type or range for its key (such as `read_only = "yes"` or `port = 70000`). | |
| `password` | A `password` key. | `key` |
| `unsupported-version` | `dbento` is an integer other than 1. | |
| `invalid-name` | A connection name outside `[A-Za-z0-9_-]+`. | `name` |
| `invalid` | A connection's keys are wrong for its engine. | `connection` |
| `invalid-folder` | A `[folders]` path or colour is wrong. | `folder` |
| `secret` | A secret in a MongoDB `uri` or a DynamoDB `endpoint`. | `connection`, `key` |

### Precedence

Reading stops at the first fault, checked in this order:

1. TOML syntax (`parse`).
2. The `dbento` version (`parse` or `unsupported-version`).
3. A `password` key anywhere (`password`), so it wins over an unknown key.
4. The typed shape with unknown keys refused (`parse`).
5. Folders, in key order (`invalid-folder`).
6. Connections, in name order. Each checks its name (`invalid-name`), then its keys for the engine
   (`invalid`), then the `uri` or `endpoint` (`invalid` or `secret`). A MongoDB `secret` is
   checked after the `uri`, whose user it depends on.

The `endpoint` check tests that the value parses and is `http` or `https` before it looks for a
user, so `ftp://u:p@h` reports `invalid`, not `secret`.

## Fixtures

`fixtures/` holds example workspaces that the crate's tests read with `Workspace::from_text`:

- `fixtures/valid/<case>/dbento.toml` opens. Its `expect.toml` holds one
  `[connections.<name>]` table per connection, with `read_only = <bool>` and, when the effective
  colour exists, `color = "<colour>"`.
- `fixtures/invalid/<case>/dbento.toml` fails. Its `expect.toml` holds `code = "<code>"`, plus
  `key = "<dotted key>"` and `connection = "<name>"` where the error names them. `connection`
  is compared with the connection of `invalid` and `secret`, and with the name of `invalid-name`.

Each invalid case holds exactly one fault, except `password-wins`, which pins step 3 of the
precedence, and `dynamodb-endpoint-scheme-first`, which pins the endpoint order. No fixture holds
a real secret: any password-shaped value is the literal `x`.
