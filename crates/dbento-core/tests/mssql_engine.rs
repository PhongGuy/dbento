//! The SQL Server engine as the workspace, capability and dialect layers see it, before any server.
use dbento_core::{
    ConnectionSpec, Edit, EditValue, Engine, ExecuteError, ExplainError, ExplainMode, Flavor,
    FormatOptions, MemoryStore, OpenError, SecretSource, TlsMode, Workspace, default_schema,
    explain_request, explain_text, format_sql, key_filter, line_comment_starts, plan_edit,
    quote_ident, quote_literal, secret_hint, single_source, splits_token, statement_ranges,
};

fn ws_with(toml: &str) -> (tempfile::TempDir, Result<Workspace, OpenError>) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path());
    (dir, ws)
}

fn invalid(toml: &str) -> String {
    match ws_with(toml).1 {
        Err(OpenError::Invalid { message, .. }) => message,
        other => panic!("{:?}", other.err()),
    }
}

const MS: &str =
    "dbento = 1\n[connections.ms]\nengine = \"mssql\"\nhost = \"db.example.com\"\nuser = \"app\"\n";

#[test]
fn mssql_and_its_sqlserver_alias_name_one_engine() {
    let (_dir, ws) = ws_with(&format!(
        "{MS}[connections.alias]\nengine = \"sqlserver\"\nhost = \"h\"\nuser = \"u\"\ndatabase = \"shop\"\n"
    ));
    let ws = ws.unwrap();
    let c = ws.connection("ms").unwrap();
    assert_eq!((c.engine(), c.engine().name()), (Engine::Mssql, "mssql"));
    assert_eq!(
        (c.host(), c.port(), c.user(), c.database()),
        (Some("db.example.com"), Some(1433), Some("app"), None)
    );
    assert_eq!(c.display_target(), "app@db.example.com:1433");
    let alias = ws.connection("alias").unwrap();
    assert_eq!(alias.engine(), Engine::Mssql);
    assert_eq!(alias.display_target(), "u@h:1433/shop");
    assert_eq!(ws.connection_spec("alias").unwrap().engine, Engine::Mssql);
    let m = invalid("dbento = 1\n[connections.x]\nengine = \"oracle\"\nhost = \"h\"\n");
    assert!(
        m.contains("unknown engine `oracle`") && m.contains("mssql"),
        "{m}"
    );
}

#[test]
fn sql_server_capabilities_and_flavor() {
    let ms = Engine::Mssql.capabilities();
    assert!(ms.schemas && ms.server_cancel && ms.password && ms.tls);
    assert!(!ms.file && !ms.edit_rows && !ms.explain && !ms.table_definition);
    assert_ne!(Flavor::SqlServer, Flavor::Mysql);
}

#[test]
fn a_sql_server_keychain_connection_gets_the_secret_hint() {
    let (_dir, ws) = ws_with(MS);
    let ws = ws.unwrap();
    let hint = secret_hint(&ws, ws.connection("ms").unwrap(), &MemoryStore::new()).unwrap();
    assert!(
        hint.contains("dbento secret set ms") && hint.contains("DBENTO_SECRET_MS"),
        "{hint}"
    );
}

#[test]
fn names_and_literals_are_t_sql() {
    assert_eq!(quote_ident(Engine::Mssql, "users"), "[users]");
    assert_eq!(quote_ident(Engine::Mssql, "a]b"), "[a]]b]");
    assert_eq!(quote_literal(Engine::Mssql, "it's"), "N'it''s'");
    assert_eq!(quote_literal(Engine::Mssql, "a\\b é"), "N'a\\b é'");
    let bytes_key = [("k".to_owned(), EditValue::Bytes(vec![1, 2]))];
    assert_eq!(key_filter(Engine::Mssql, &bytes_key), None);
    let (_dir, ws) = ws_with(MS);
    let ws = ws.unwrap();
    let schema = default_schema(Engine::Mssql, ws.connection("ms").unwrap());
    assert_eq!(schema.as_deref(), Some("dbo"));
}

fn src(sql: &str) -> Option<dbento_core::RelationRef> {
    single_source(Engine::Mssql, sql, Some("dbo"))
}

#[test]
fn a_plain_t_sql_select_names_its_table() {
    use dbento_core::RelationRef;
    for (sql, schema, name) in [
        ("SELECT * FROM users", "dbo", "users"),
        (
            "SELECT TOP (5) id FROM sales.Orders o WHERE id > 1 ORDER BY id",
            "sales",
            "Orders",
        ),
        (
            "select a from [my schema].[odd]]name];",
            "my schema",
            "odd]name",
        ),
        ("SELECT a FROM \"Sales\".\"Orders\"", "Sales", "Orders"),
        ("SELECT a FROM t WITH (NOLOCK)", "dbo", "t"),
        ("SELECT a FROM [#t]", "dbo", "#t"),
    ] {
        assert_eq!(src(sql), Some(RelationRef::new(schema, name)), "{sql}");
    }
}

#[test]
fn a_t_sql_select_that_is_not_one_plain_table_names_nothing() {
    for sql in [
        "SELECT 1",
        "SELECT a FROM t JOIN u ON t.a = u.a",
        "SELECT a FROM t, u",
        "SELECT a FROM (SELECT a FROM t) s",
        "SELECT a FROM t WHERE a IN (SELECT a FROM u)",
        "WITH c AS (SELECT 1 AS a) SELECT a FROM c",
        "SELECT a INTO x FROM t",
        "SELECT a FROM t UNION SELECT a FROM u",
        "SELECT a FROM t FOR XML PATH",
        "SELECT a FROM t FOR JSON AUTO",
        "SELECT a FROM other.dbo.t",
        "SELECT a FROM srv.other.dbo.t",
        "SELECT a FROM #t",
        "SELECT a FROM ##t",
        "SELECT a FROM @t",
        "SELECT a FROM dbo.f(1)",
        "SELECT a FROM OPENJSON(@j)",
        "SELECT 1 AS a SELECT 2 AS b",
        "SELECT a FROM t; SELECT b FROM u",
        "DELETE FROM t",
        "SELECT a FROM",
        "SELECT 'select' FROM /* SELECT */ t -- SELECT",
    ] {
        let want = sql
            .contains("'select'")
            .then(|| dbento_core::RelationRef::new("dbo", "t"));
        assert_eq!(src(sql), want, "{sql}");
    }
    assert_eq!(single_source(Engine::Mssql, "SELECT a FROM t", None), None);
}

fn ms(sql: &str) -> String {
    format_sql(Engine::Mssql, sql, &FormatOptions::default())
}

#[test]
fn the_formatter_lays_out_t_sql_and_keeps_its_tokens_whole() {
    assert_eq!(
        ms("select [a b], N'x''y' from [dbo].[t]] u] /* x /* y */ z */ where a<>@p and b=1.5e3"),
        "SELECT [a b],\n       N'x''y'\nFROM [dbo].[t]] u] /* x /* y */ z */\nWHERE a<>@p\n      AND b=1.5e3"
    );
    assert_eq!(ms("select top (5) a from t"), "SELECT top (5) a\nFROM t");
    assert_eq!(
        ms("select #t.a, $5, 1.5, .5 from #t"),
        "SELECT #t.a,\n       $5,\n       1.5,\n       .5\nFROM #t"
    );
}

/// The formatter's keyword list split by Microsoft's reserved keyword table.
const NOT_RESERVED: &str = "after before conflict do each explain false filter following ilike interval language lateral limit locked natural next nothing nowait nulls offset only partition preceding recursive replace returning returns row rows skip true unbounded using window within";
const RESERVED: &str = "all alter and any as asc begin between by case commit constraint create cross current default delete desc distinct drop else end escape except execute exists fetch for foreign from full function group having if in index inner insert intersect into is join left like not null on or order outer over primary references right rollback select set some table then trigger union unique update values view when where with";

#[test]
fn a_case_sensitive_database_keeps_every_word_it_could_read_as_a_name() {
    // None of these is reserved in T-SQL, so each may be a bare column on a CS collation.
    for word in NOT_RESERVED.split_whitespace() {
        let formatted = ms(&format!("select a {word} b from t"));
        assert!(formatted.contains(word), "{formatted}");
    }
    assert_eq!(
        ms("select Rows, Next from t"),
        "SELECT Rows,
       Next
FROM t"
    );
}

#[test]
fn reserved_words_are_recased_and_key_only_in_a_key_clause() {
    for word in RESERVED.split_whitespace() {
        let upper = word.to_ascii_uppercase();
        let formatted = ms(&format!("select a {word} b from t"));
        assert!(formatted.contains(&upper), "{formatted}");
    }
    assert_eq!(
        ms("create table t (id int primary key)"),
        "CREATE TABLE t (id int PRIMARY KEY)"
    );
    assert_eq!(ms("alter table t add key b"), "ALTER TABLE t add key b");
}

#[test]
fn go_lines_stay_their_own_lines_and_each_batch_is_laid_out_alone() {
    assert_eq!(
        ms("select a from t\nGO\n\nselect b from u\ngo 3 -- twice more\nselect c from v"),
        "SELECT a\nFROM t\nGO\n\nSELECT b\nFROM u\ngo 3 -- twice more\nSELECT c\nFROM v"
    );
}

// The layout would leave `go` alone on its line, which is a batch separator.
#[test]
fn a_column_named_go_stays_in_its_batch() {
    for sql in ["select a, go from t", "select a, go -- c\nfrom t"] {
        assert_eq!(ms(sql), sql);
        assert_eq!(statement_ranges(Engine::Mssql, &ms(sql)).len(), 1, "{sql}");
    }
}

// GO is not reserved, so a case-sensitive database may read it as a name in any case.
#[test]
fn a_column_named_go_is_not_recased() {
    assert_eq!(ms("SELECT a, GO FROM t"), "SELECT a, GO FROM t");
    assert_eq!(
        ms("select a, Go, b from t"),
        "SELECT a,\n       Go,\n       b\nFROM t"
    );
}

// A backslash before a newline continues a binary constant, so that newline is code.
#[test]
fn a_continued_binary_constant_keeps_its_line_break() {
    let sql = "select 0xAB\\\nCD from t";
    assert_eq!(ms(sql), sql);
    assert_eq!(
        ms("select 'a\\\nb' from t"),
        "SELECT 'a\\\nb'\nFROM t",
        "inside a string the same two characters are text"
    );
}

// `@` leads a word to the T-SQL lexer, and no variable is a reserved word.
#[test]
fn a_variable_is_never_recased() {
    assert_eq!(
        ms("select @Rows, @select, @@RowCount from t"),
        "SELECT @Rows,\n       @select,\n       @@RowCount\nFROM t"
    );
}

#[test]
fn a_script_keeps_its_batches_and_counts_through_the_formatter() {
    let sql = "select a from t\nGO 3\nselect b from u\ngo";
    let formatted = ms(sql);
    assert_eq!(formatted, "SELECT a\nFROM t\nGO 3\nSELECT b\nFROM u\ngo");
    assert_eq!(statement_ranges(Engine::Mssql, &formatted).len(), 2);
    // One batch the layout would split sends the whole script back, its real GO included.
    let mixed = "select a, go from t\nGO 3\nselect b from u";
    assert_eq!(ms(mixed), mixed);
}

#[test]
fn unterminated_t_sql_comes_back_unchanged() {
    for sql in [
        "select 'a",
        "select N'a",
        "select [a",
        "select \"a",
        "select 1 /* a /* b */",
    ] {
        assert_eq!(ms(sql), sql);
        assert_eq!(line_comment_starts(Engine::Mssql, sql), None);
    }
}

#[test]
fn comment_toggling_and_selection_splits_read_t_sql_tokens() {
    let sql = "select [a -- b] -- c\n, N'-- d' /* -- e /* f */ -- g */ -- h";
    assert_eq!(line_comment_starts(Engine::Mssql, sql), Some(vec![16, 55]));
    let name = sql.find("-- b").unwrap();
    assert!(splits_token(Engine::Mssql, sql, name..name + 2));
    assert!(!splits_token(Engine::Mssql, sql, 0..6));
    assert!(
        !splits_token(Engine::Mssql, "select 'a", 0..6),
        "text past the range is not read"
    );
    assert!(splits_token(Engine::Mssql, "select 'a", 0..9));
}

#[test]
fn explain_leaves_sql_server_alone() {
    let explained = explain_request(Engine::Mssql, "SELECT 1", ExplainMode::Plan);
    assert_eq!(explained.err(), Some(ExplainError::UnsupportedEngine));
    assert_eq!(
        explain_text(Engine::Mssql, "SELECT 1").err(),
        Some(ExplainError::UnsupportedEngine)
    );
}

#[test]
fn sql_server_keys_parse_with_their_defaults() {
    let (_dir, ws) = ws_with(
        "dbento = 1\n[connections.ms]\nengine = \"mssql\"\nhost = \"127.0.0.1\"\nuser = \"app\"\n\
         database = \"shop\"\ntls_ca = \"ca.pem\"\nread_only_user = \"dbento_reader\"\nsecret = { env = \"MS_PW\" }\n",
    );
    let ws = ws.unwrap();
    let c = ws.connection("ms").unwrap();
    assert_eq!((c.port(), c.database()), (Some(1433), Some("shop")));
    assert_eq!(
        c.tls(),
        Some(TlsMode::Verify),
        "verify even on a local host"
    );
    assert!(c.tls_ca().is_some_and(|p| p.ends_with("ca.pem")));
    assert_eq!(c.read_only_user(), Some("dbento_reader"));
    assert_eq!(c.secret_source(), &SecretSource::Env("MS_PW".into()));
    let (_d2, plain) = ws_with(MS);
    assert_eq!(
        plain.unwrap().connection("ms").unwrap().read_only_user(),
        None
    );
}

#[test]
fn sql_server_refuses_keys_it_cannot_honour_naming_each() {
    let base = "dbento = 1\n[connections.ms]\nengine = \"mssql\"\nhost = \"h\"\nuser = \"u\"\n";
    for (line, key) in [
        ("path = \"x.db\"", "path"),
        ("settings = { a = \"b\" }", "settings"),
        ("tls_cert = \"c.pem\"\ntls_key = \"k.pem\"", "tls_cert"),
        ("tls_key = \"k.pem\"", "tls_key"),
    ] {
        let m = invalid(&format!("{base}{line}\n"));
        assert!(
            m.contains(&format!("`{key}`")) && m.contains("mssql"),
            "{key}: {m}"
        );
    }
    match ws_with(&format!("{base}tls_key_passphrase = \"x\"\n")).1 {
        Err(OpenError::Parse { message, .. }) => {
            assert!(message.contains("tls_key_passphrase"), "{message}")
        }
        other => panic!("{:?}", other.err()),
    }
    let head = "dbento = 1\n[connections.ms]\nengine = \"mssql\"\n";
    assert!(invalid(&format!("{head}user = \"u\"\n")).contains("`host`"));
    assert!(invalid(&format!("{head}host = \"h\"\n")).contains("`user`"));
    assert!(invalid(&format!("{base}read_only_user = \"\"\n")).contains("`read_only_user`"));
    assert!(invalid(&format!("{base}read_only_user = \"  \"\n")).contains("`read_only_user`"));
    assert!(invalid(&format!("{base}tls = \"strict\"\n")).contains("tls `strict`"));
}

#[test]
fn read_only_user_is_a_sql_server_key_only() {
    for (engine, keys) in [
        ("postgres", "host = \"h\"\ndatabase = \"d\"\nuser = \"u\"\n"),
        ("mysql", "host = \"h\"\nuser = \"u\"\n"),
        ("sqlite", "path = \"a.db\"\n"),
    ] {
        let m = invalid(&format!(
            "dbento = 1\n[connections.c]\nengine = \"{engine}\"\n{keys}read_only_user = \"r\"\n"
        ));
        assert!(
            m.contains("`read_only_user`") && m.contains(engine),
            "{engine}: {m}"
        );
    }
}

#[test]
fn a_sql_server_entry_round_trips_through_a_save() {
    let (_dir, ws) = ws_with("dbento = 1\n");
    let ws = ws.unwrap();
    let mut spec = ConnectionSpec::new(Engine::Mssql);
    spec.host = Some("db.example.com".into());
    spec.port = Some(14330);
    spec.database = Some("shop".into());
    spec.user = Some("app".into());
    spec.tls = Some(TlsMode::Off);
    spec.tls_ca = Some("certs/ca.pem".into());
    spec.read_only_user = Some("dbento_reader".into());
    spec.secret = Some(SecretSource::Keychain);
    spec.read_only = Some(true);
    let edit = Edit::PutConnection {
        previous: None,
        name: "ms".into(),
        spec: spec.clone(),
    };
    let text = plan_edit(&ws, &edit).unwrap().new_text().to_owned();
    assert!(text.contains("engine = \"mssql\""), "{text}");
    assert!(
        text.contains("read_only_user = \"dbento_reader\""),
        "{text}"
    );
    let (_d2, reopened) = ws_with(&text);
    let reopened = reopened.unwrap();
    assert_eq!(reopened.connection_spec("ms"), Some(spec));
    let c = reopened.connection("ms").unwrap();
    assert_eq!(
        (c.read_only_user(), c.port(), c.tls()),
        (Some("dbento_reader"), Some(14330), Some(TlsMode::Off))
    );
}

#[test]
fn a_drain_cap_stop_on_sql_server_does_not_promise_a_rollback() {
    let m = ExecuteError::StoppedAtDrainCap.message_for(Engine::Mssql);
    assert!(!m.contains("any writes it made were rolled back"), "{m}");
    assert!(m.contains("already finished") && m.contains("keep"), "{m}");
    let e = ExecuteError::StoppedAtDrainCap;
    assert_eq!(e.message_for(Engine::Postgres), e.to_string());
}

#[test]
fn a_sql_server_stop_says_finished_statements_may_have_kept_their_writes() {
    for e in [ExecuteError::Cancelled, ExecuteError::Timeout] {
        let m = e.message_for(Engine::Mssql);
        assert!(m.starts_with(&e.to_string()), "{m}");
        assert!(
            m.contains("already finished") && m.contains("may have kept"),
            "{m}"
        );
        assert_eq!(e.message_for(Engine::Postgres), e.to_string());
    }
}
