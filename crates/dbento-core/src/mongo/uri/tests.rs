fn driver_refused(uri: &str) -> String {
    super::driver_syntax(uri)
        .err()
        .unwrap_or_else(|| panic!("{uri} accepted by the driver"))
}

// User and database as an opened workspace reads them from the uri.
fn user_and_database(uri: &str) -> (Option<String>, String) {
    let text = format!("dbento = 1\n[connections.c]\nengine = \"mongodb\"\nuri = \"{uri}\"\n");
    let ws = crate::workspace::Workspace::from_text(
        std::path::PathBuf::from("/ws"),
        std::path::Path::new("/ws/dbento.toml"),
        &text,
    )
    .unwrap_or_else(|e| panic!("{uri}: {e}"));
    let c = ws.declared("c").unwrap();
    (
        c.user().map(str::to_owned),
        c.database().unwrap().to_owned(),
    )
}

#[test]
fn the_driver_checks_the_syntax_and_agrees_on_user_and_database() {
    use mongodb::options::ConnectionString;
    let m = driver_refused("mongodb://h:x/d");
    assert!(
        m.starts_with("`uri` is not a MongoDB connection string: ")
            && m.contains("the port must be an integer"),
        "{m}"
    );
    assert_eq!(
        driver_refused("mongodb://u@h:x/d"),
        "`uri` is not a MongoDB connection string (the driver's reason is hidden because the text may hold a password)"
    );
    for uri in [
        "mongodb://app@db.internal:27017/shop?authSource=admin",
        "mongodb://localhost",
        "mongodb://app%40corp@h1,h2/?replicaSet=rs",
        "mongodb://h/my%2Ddb",
        "mongodb+srv://app@cluster0.example.net/",
        "mongodb://u@h/d?appName=a@b",
        "mongodb://[::1]:27017/",
        "mongodb://u%3Ap@h",
    ] {
        super::driver_syntax(uri).unwrap_or_else(|e| panic!("{uri}: {e}"));
        let d = ConnectionString::parse(uri).unwrap();
        let user = d.credential.and_then(|c| c.username);
        let database = d.default_database.unwrap_or_else(|| "test".to_owned());
        assert_eq!(user_and_database(uri), (user, database), "{uri}");
    }
}

#[test]
fn a_driver_reason_that_can_quote_the_text_stays_hidden() {
    let hidden = "`uri` is not a MongoDB connection string (the driver's reason is hidden because the text may hold a password)";
    let port =
        "`uri` is not a MongoDB connection string: the port must be an integer between 1 and 65535";
    for (uri, want) in [
        ("mongodb://app:hunter2/shop", port),
        ("mongodb://app:hunter2%40h/", port),
        ("mongodb://[hunter2/", hidden),
        ("mongodb://h/?hunter2", hidden),
        ("mongodb://h/?readPreference=hunter2", hidden),
    ] {
        assert_eq!(driver_refused(uri), want, "{uri}");
    }
}

#[test]
fn the_driver_reads_proxy_options_once_socks5_is_on() {
    for uri in [
        "mongodb://h/?proxyHost=127.0.0.1",
        "mongodb://h/?proxyHost=127.0.0.1&proxyPort=1080&proxyUsername=u&proxyPassword=p",
    ] {
        assert_eq!(super::driver_syntax(uri), Ok(()), "{uri}");
    }
}
