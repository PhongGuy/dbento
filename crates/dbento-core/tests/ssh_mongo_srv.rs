//! `mongodb+srv` through the bastion: SRV and TXT answered here, members reached over SSH.
#![cfg(feature = "mongodb")]

use std::net::SocketAddr;

use crate::ssh_mongo::mg_tunnel;
use crate::ssh_support::{Tunnel, password};
use crate::support::read;
use dbento_core::{ConnectError, Session, SessionOptions, connect};
use dbento_testkit::{MG_ZONE, SSH_PASSWORD, mongo_on, ssh_net};
use hickory_proto::op::{Message, ResponseCode};
use hickory_proto::rr::rdata::{SRV, TXT};
use hickory_proto::rr::{Name, RData, Record, RecordType};

/// A UDP nameserver on loopback answering `_mongodb._tcp.<srv>` with `targets` on 27017 and
/// `<srv>` TXT with `txt`; every other question, or an empty answer, gets NXDOMAIN.
async fn dns_responder(srv: &str, targets: Vec<String>, txt: &str) -> SocketAddr {
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    let srv_name = format!("_mongodb._tcp.{srv}");
    let (srv, txt) = (srv.to_owned(), txt.to_owned());
    tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        while let Ok((n, peer)) = socket.recv_from(&mut buf).await {
            let Ok(query) = Message::from_vec(&buf[..n]) else {
                continue;
            };
            let mut reply = Message::response(query.metadata.id, query.metadata.op_code);
            reply.metadata.recursion_desired = query.metadata.recursion_desired;
            reply.metadata.recursion_available = true;
            reply.metadata.authoritative = true;
            for q in &query.queries {
                reply.add_query(q.clone());
                let asked = q.name().to_ascii();
                let asked = asked.trim_end_matches('.');
                if q.query_type() == RecordType::SRV && asked.eq_ignore_ascii_case(&srv_name) {
                    for t in &targets {
                        let target = Name::from_ascii(format!("{t}.")).unwrap();
                        let rdata = RData::SRV(SRV::new(0, 0, 27017, target));
                        reply.add_answer(Record::from_rdata(q.name().clone(), 60, rdata));
                    }
                }
                if q.query_type() == RecordType::TXT
                    && asked.eq_ignore_ascii_case(&srv)
                    && !txt.is_empty()
                {
                    let rdata = RData::TXT(TXT::new(vec![txt.clone()]));
                    reply.add_answer(Record::from_rdata(q.name().clone(), 60, rdata));
                }
            }
            if reply.answers.is_empty() {
                reply.metadata.response_code = ResponseCode::NXDomain;
            }
            let _ = socket.send_to(&reply.to_vec().unwrap(), peer).await;
        }
    });
    addr
}

async fn srv_session(t: &Tunnel, dns: SocketAddr) -> Result<Session, ConnectError> {
    let mut opts = SessionOptions::default();
    opts.ssh_env = Some(t.env());
    opts.ssh_secrets = password(SSH_PASSWORD);
    opts.mongo_dns = Some(dns);
    connect(&t.ws, t.ws.declared("mg").unwrap(), None, opts).await
}

#[tokio::test]
async fn an_srv_uri_resolves_here_and_reaches_its_members_through_the_tunnel() {
    let net = ssh_net().await;
    let mg = mongo_on(net.network(), 2).await;
    mg.eval(
        0,
        "db.getSiblingDB('app').ssh.insertOne({k: 's'}, {writeConcern: {w: 2}})",
    )
    .await;
    let srv = format!("rs-{}.{MG_ZONE}", std::process::id());
    let dns = dns_responder(&srv, mg.names().to_vec(), "replicaSet=rs0").await;
    let uri = format!(
        "mongodb+srv://{srv}/app?tlsCAFile=ca.pem&serverSelectionTimeoutMS=4000&readPreference=secondary"
    );
    let t = mg_tunnel(&net, &uri, mg.ca_pem());
    let mut s = srv_session(&t, dns).await.unwrap();
    assert_eq!(read(&mut s, "db.ssh.find()").await.rows.len(), 1);
}

#[tokio::test]
async fn an_srv_name_that_does_not_resolve_here_says_where_it_was_looked_up() {
    let net = ssh_net().await;
    let srv = format!("gone-{}.{MG_ZONE}", std::process::id());
    let dns = dns_responder(&srv, Vec::new(), "").await;
    let t = mg_tunnel(&net, &format!("mongodb+srv://{srv}/app"), "");
    let err = srv_session(&t, dns)
        .await
        .err()
        .expect("an unresolvable SRV name connected");
    let text = err.to_string();
    assert!(
        text.contains("resolved on this machine, not on the SSH server"),
        "{text}"
    );
    assert!(text.contains("mongodb://"), "{text}");
}
