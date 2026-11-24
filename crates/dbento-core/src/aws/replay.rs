//! A scripted HTTP client for the AWS SDKs: it records each request and answers from a queue,
//! so paging, retries, deadlines and error bodies are tested without a server. Engines layer
//! their own wire format (JSON targets, XML bodies) on top.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aws_smithy_runtime_api::client::http::{
    HttpClient, HttpConnector, HttpConnectorFuture, HttpConnectorSettings, SharedHttpClient,
    SharedHttpConnector,
};
use aws_smithy_runtime_api::client::orchestrator::{HttpRequest, HttpResponse};
use aws_smithy_runtime_api::client::result::ConnectorError;
use aws_smithy_runtime_api::client::runtime_components::RuntimeComponents;
use aws_smithy_runtime_api::http::StatusCode;
use aws_smithy_types::body::SdkBody;

/// One scripted answer and how long to wait before giving it.
#[derive(Clone)]
pub(crate) struct RawReply {
    answer: Answer,
    delay: Duration,
}

/// What the scripted server does: answer with a status, headers and a body, or let the
/// connector time out before any response arrives.
#[derive(Clone)]
enum Answer {
    Http {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    DispatchTimeout,
}

impl RawReply {
    /// An answer with `status`, no headers and an empty body.
    pub(crate) fn status(status: u16) -> Self {
        RawReply {
            answer: Answer::Http {
                status,
                headers: Vec::new(),
                body: Vec::new(),
            },
            delay: Duration::ZERO,
        }
    }

    pub(crate) fn header(mut self, name: &str, value: &str) -> Self {
        if let Answer::Http { headers, .. } = &mut self.answer {
            headers.push((name.to_owned(), value.to_owned()));
        }
        self
    }

    pub(crate) fn body(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        if let Answer::Http { body, .. } = &mut self.answer {
            *body = bytes.into();
        }
        self
    }

    /// The body as an XML document, with the content type S3 sends.
    pub(crate) fn xml(self, body: &str) -> Self {
        self.header("content-type", "application/xml").body(body)
    }

    /// A connector timeout, as the SDK sees a connect or read that never finished.
    pub(crate) fn dispatch_timeout() -> Self {
        RawReply {
            answer: Answer::DispatchTimeout,
            delay: Duration::ZERO,
        }
    }

    /// The same answer, sent only after `delay`.
    pub(crate) fn after(self, delay: Duration) -> Self {
        RawReply { delay, ..self }
    }
}

/// A request as the server saw it.
#[derive(Debug, Clone)]
pub(crate) struct RawSeen {
    pub(crate) method: String,
    pub(crate) uri: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl RawSeen {
    /// The first value of the header `name`, matched without regard to case.
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// The scripted client; clones share the queue and the record.
#[derive(Clone, Default)]
pub(crate) struct RawReplay {
    replies: Arc<Mutex<VecDeque<RawReply>>>,
    seen: Arc<Mutex<Vec<RawSeen>>>,
}

impl fmt::Debug for RawReplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RawReplay")
    }
}

impl RawReplay {
    pub(crate) fn new(replies: Vec<RawReply>) -> Self {
        RawReplay {
            replies: Arc::new(Mutex::new(replies.into())),
            seen: Arc::default(),
        }
    }

    /// Every request so far, in order.
    pub(crate) fn seen(&self) -> Vec<RawSeen> {
        self.seen.lock().unwrap().clone()
    }

    pub(crate) fn http(&self) -> SharedHttpClient {
        SharedHttpClient::new(self.clone())
    }
}

impl HttpConnector for RawReplay {
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        let seen = RawSeen {
            method: request.method().to_owned(),
            uri: request.uri().to_owned(),
            headers: request
                .headers()
                .iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
            body: request.body().bytes().unwrap_or_default().to_vec(),
        };
        self.seen.lock().unwrap().push(seen);
        let next = self.replies.lock().unwrap().pop_front();
        HttpConnectorFuture::new(async move {
            let Some(reply) = next else {
                return Err(ConnectorError::other(
                    "the replay ran out of replies".into(),
                    None,
                ));
            };
            tokio::time::sleep(reply.delay).await;
            let (status, headers, body) = match reply.answer {
                Answer::Http {
                    status,
                    headers,
                    body,
                } => (status, headers, body),
                Answer::DispatchTimeout => {
                    return Err(ConnectorError::timeout(
                        "the replay sent nothing before the deadline".into(),
                    ));
                }
            };
            let status = StatusCode::try_from(status).expect("a valid status");
            let mut response = HttpResponse::new(status, SdkBody::from(body));
            for (name, value) in headers {
                response.headers_mut().append(name, value);
            }
            Ok(response)
        })
    }
}

impl HttpClient for RawReplay {
    fn http_connector(
        &self,
        _: &HttpConnectorSettings,
        _: &RuntimeComponents,
    ) -> SharedHttpConnector {
        SharedHttpConnector::new(self.clone())
    }
}

#[cfg(test)]
mod tests;
