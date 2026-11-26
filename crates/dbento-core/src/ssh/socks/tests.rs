use super::{Refusal, Target, negotiate, reply};
use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

const USER: &str = "user-0123456789";
const PASS: &str = "pass-0123456789";

// The client writes `sent` and shuts its write half, so a server wanting more reads EOF.
async fn exchange(sent: &[u8]) -> (Result<Target, Refusal>, Vec<u8>) {
    let (mut client, mut server) = duplex(4096);
    client.write_all(sent).await.unwrap();
    client.shutdown().await.unwrap();
    let outcome = negotiate(&mut server, USER, PASS).await;
    drop(server);
    let mut back = Vec::new();
    client.read_to_end(&mut back).await.unwrap();
    (outcome, back)
}

fn greeting(methods: &[u8]) -> Vec<u8> {
    [
        vec![5, u8::try_from(methods.len()).unwrap()],
        methods.to_vec(),
    ]
    .concat()
}

fn login(user: &[u8], pass: &[u8]) -> Vec<u8> {
    let mut v = vec![1, u8::try_from(user.len()).unwrap()];
    v.extend_from_slice(user);
    v.push(u8::try_from(pass.len()).unwrap());
    v.extend_from_slice(pass);
    v
}

fn request(cmd: u8, atyp: u8, addr: &[u8], port: u16) -> Vec<u8> {
    [
        vec![5, cmd, 0, atyp],
        addr.to_vec(),
        port.to_be_bytes().to_vec(),
    ]
    .concat()
}

fn domain(name: &[u8]) -> Vec<u8> {
    [vec![u8::try_from(name.len()).unwrap()], name.to_vec()].concat()
}

fn logged_in() -> Vec<u8> {
    [greeting(&[0, 2]), login(USER.as_bytes(), PASS.as_bytes())].concat()
}

const fn refused(rep: u8) -> [u8; 10] {
    [5, rep, 0, 1, 0, 0, 0, 0, 0, 0]
}

#[tokio::test]
async fn a_domain_connect_with_the_right_login_yields_its_target() {
    let sent = [
        logged_in(),
        request(1, 3, &domain(b"m1.dbento.test"), 27017),
    ]
    .concat();
    let (outcome, back) = exchange(&sent).await;
    assert_eq!(
        outcome.unwrap(),
        Target {
            host: "m1.dbento.test".to_owned(),
            port: 27017
        }
    );
    assert_eq!(back, [5, 2, 1, 0]);
}

#[tokio::test]
async fn ip_targets_become_their_literals() {
    let v4 = [logged_in(), request(1, 1, &[10, 0, 0, 7], 5432)].concat();
    assert_eq!(exchange(&v4).await.0.unwrap().host, "10.0.0.7");
    let mut loopback = [0u8; 16];
    loopback[15] = 1;
    let v6 = [logged_in(), request(1, 4, &loopback, 5432)].concat();
    assert_eq!(exchange(&v6).await.0.unwrap().host, "::1");
}

#[tokio::test]
async fn a_greeting_without_password_auth_is_refused_with_ff() {
    let (outcome, back) = exchange(&greeting(&[0])).await;
    assert!(
        matches!(outcome, Err(Refusal::NoAcceptableMethod)),
        "{outcome:?}"
    );
    assert_eq!(back, [5, 0xFF]);
}

#[tokio::test]
async fn http_bytes_are_closed_without_a_reply() {
    let (outcome, back) = exchange(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").await;
    assert!(matches!(outcome, Err(Refusal::NotSocks5)), "{outcome:?}");
    assert!(back.is_empty(), "{back:?}");
}

#[tokio::test]
async fn a_wrong_or_empty_login_gets_01_01() {
    let short = &PASS.as_bytes()[..PASS.len() - 1];
    for (user, pass) in [
        (USER.as_bytes(), b"wrong".as_slice()),
        (b"wrong".as_slice(), PASS.as_bytes()),
        (USER.as_bytes(), short),
        (b"".as_slice(), PASS.as_bytes()),
    ] {
        let sent = [greeting(&[0, 2]), login(user, pass)].concat();
        let (outcome, back) = exchange(&sent).await;
        assert!(matches!(outcome, Err(Refusal::BadLogin)), "{outcome:?}");
        assert_eq!(back, [5, 2, 1, 1]);
    }
}

#[tokio::test]
async fn bind_bad_address_types_bad_domains_and_port_zero_get_their_codes() {
    let cases: [(Vec<u8>, u8); 5] = [
        (request(2, 3, &domain(b"h"), 1), 0x07),
        (vec![5, 1, 0, 9], 0x08),
        (request(1, 3, &domain(&[0xFF, 0xFE]), 1), 0x01),
        (request(1, 3, &domain(b"a\0b"), 1), 0x01),
        (request(1, 3, &domain(b"h"), 0), 0x02),
    ];
    for (req, rep) in cases {
        let (outcome, back) = exchange(&[logged_in(), req].concat()).await;
        assert!(outcome.is_err(), "rep {rep}: {outcome:?}");
        assert_eq!(back[4..], refused(rep), "rep {rep}");
    }
}

#[tokio::test]
async fn a_truncated_handshake_is_an_io_refusal() {
    let (outcome, back) = exchange(&[5]).await;
    assert!(matches!(outcome, Err(Refusal::Io(_))), "{outcome:?}");
    assert!(back.is_empty());
}

#[tokio::test]
async fn the_success_reply_has_the_fixed_shape() {
    let (mut client, mut server) = duplex(64);
    reply(&mut server, super::REP_SUCCESS).await.unwrap();
    drop(server);
    let mut back = Vec::new();
    client.read_to_end(&mut back).await.unwrap();
    assert_eq!(back, refused(0));
}

mod endpoint {
    use super::super::SocksEndpoint;
    use crate::ssh::Dialer;
    use fast_socks5::client::{Config, Socks5Stream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// An echo server on loopback counting the connections it accepted.
    async fn echo() -> (u16, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&accepted);
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                seen.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let (mut r, mut w) = s.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });
        (port, accepted)
    }

    async fn through(
        ep: &SocksEndpoint,
        host: &str,
        port: u16,
        pass: &str,
    ) -> Result<Socks5Stream<TcpStream>, fast_socks5::SocksError> {
        let (user, _) = ep.credentials();
        Socks5Stream::connect_with_password(
            format!("127.0.0.1:{}", ep.port()),
            host.to_owned(),
            port,
            user.to_owned(),
            pass.to_owned(),
            Config::default(),
        )
        .await
    }

    #[tokio::test]
    async fn the_right_login_relays_to_the_target_by_ip_and_by_name() {
        let ep = SocksEndpoint::start(Dialer::Direct).await.unwrap();
        let (port, _) = echo().await;
        let pass = ep.credentials().1.to_owned();
        for host in ["127.0.0.1", "localhost"] {
            let mut s = through(&ep, host, port, &pass).await.unwrap();
            s.write_all(b"ping").await.unwrap();
            let mut back = [0u8; 4];
            s.read_exact(&mut back).await.unwrap();
            assert_eq!(&back, b"ping", "{host}");
        }
    }

    #[tokio::test]
    async fn it_binds_loopback_with_fresh_43_character_credentials() {
        let a = SocksEndpoint::start(Dialer::Direct).await.unwrap();
        let b = SocksEndpoint::start(Dialer::Direct).await.unwrap();
        assert_eq!(a.addr().ip(), std::net::Ipv4Addr::LOCALHOST);
        let url_safe = |s: &str| {
            s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        };
        let (user, pass) = a.credentials();
        for c in [user, pass] {
            assert_eq!(c.len(), 43, "{c}");
            assert!(url_safe(c), "{c}");
        }
        assert_ne!(user, pass);
        assert_ne!(a.credentials(), b.credentials());
    }

    #[tokio::test]
    async fn a_wrong_password_never_reaches_the_target() {
        let ep = SocksEndpoint::start(Dialer::Direct).await.unwrap();
        let (port, accepted) = echo().await;
        assert!(through(&ep, "127.0.0.1", port, "wrong").await.is_err());
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(accepted.load(Ordering::SeqCst), 0);
        assert_eq!(ep.last_refusal(), None);
    }

    #[tokio::test]
    async fn http_bytes_are_closed() {
        let ep = SocksEndpoint::start(Dialer::Direct).await.unwrap();
        let mut raw = TcpStream::connect(ep.addr()).await.unwrap();
        raw.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut back = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), raw.read_to_end(&mut back))
            .await
            .expect("the endpoint kept an HTTP client open")
            .unwrap();
        assert!(back.is_empty(), "{back:?}");
    }

    #[tokio::test]
    async fn a_refused_target_gets_rep_01_and_is_recorded() {
        let ep = SocksEndpoint::start(Dialer::Direct).await.unwrap();
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let pass = ep.credentials().1.to_owned();
        assert!(through(&ep, "127.0.0.1", port, &pass).await.is_err());
        let why = ep.last_refusal().expect("the refusal was recorded");
        assert!(!why.is_empty());
    }

    #[tokio::test]
    async fn dropping_closes_the_port_and_ends_relays() {
        let ep = SocksEndpoint::start(Dialer::Direct).await.unwrap();
        let (port, _) = echo().await;
        let pass = ep.credentials().1.to_owned();
        let mut relay = through(&ep, "127.0.0.1", port, &pass).await.unwrap();
        let addr = ep.addr();
        drop(ep);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while TcpStream::connect(addr).await.is_ok() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the port outlived the endpoint"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let mut buf = [0u8; 1];
        let ended = tokio::time::timeout(Duration::from_secs(5), relay.read(&mut buf)).await;
        assert!(matches!(ended, Ok(Ok(0) | Err(_))), "{ended:?}");
    }

    #[test]
    fn dropping_outside_a_runtime_does_not_panic() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let ep = runtime
            .block_on(SocksEndpoint::start(Dialer::Direct))
            .unwrap();
        drop(runtime);
        std::thread::spawn(move || drop(ep)).join().unwrap();
    }

    // Real time with a short deadline: paused time could fire deadlines before the
    // stalled clients are even accepted.
    #[tokio::test]
    async fn the_late_handshake_waits_for_a_slot_until_the_deadline_frees_one() {
        let deadline = Duration::from_millis(400);
        let ep = SocksEndpoint::start_with(Dialer::Direct, deadline)
            .await
            .unwrap();
        let (port, _) = echo().await;
        let mut stalled = Vec::new();
        for _ in 0..super::super::HANDSHAKE_SLOTS {
            let mut s = TcpStream::connect(ep.addr()).await.unwrap();
            s.write_all(&[5]).await.unwrap();
            stalled.push(s);
        }
        let start = tokio::time::Instant::now();
        let pass = ep.credentials().1.to_owned();
        let late = tokio::time::timeout(
            Duration::from_secs(5),
            through(&ep, "127.0.0.1", port, &pass),
        )
        .await
        .expect("the late handshake never got a slot");
        assert!(late.is_ok(), "{:?}", late.err());
        assert!(
            start.elapsed() >= deadline / 2,
            "no wait for a slot: {:?}",
            start.elapsed()
        );
        for mut s in stalled {
            let mut back = Vec::new();
            assert_eq!(
                s.read_to_end(&mut back).await.unwrap(),
                0,
                "a stalled client was kept"
            );
        }
    }
}
