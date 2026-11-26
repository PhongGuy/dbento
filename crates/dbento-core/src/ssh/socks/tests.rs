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
