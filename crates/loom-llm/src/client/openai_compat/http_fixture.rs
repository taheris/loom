#![cfg(test)]

//! Real HTTP over a fixture-owned Unix socket, with joined request handlers.

use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use tokio::net::UnixListener;
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};

use super::{ApiKey, OpenAiCompatClient, Url};

pub struct Server {
    directory: tempfile::TempDir,
    requests: Arc<Mutex<Vec<Request<Bytes>>>>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

pub fn reply(status: u16, body: impl Into<String>) -> Response<String> {
    Response::builder()
        .status(status)
        .body(body.into())
        .unwrap()
}

pub fn json_reply(body: &serde_json::Value) -> Response<String> {
    Response::builder()
        .header("content-type", "application/json")
        .body(body.to_string())
        .unwrap()
}

impl Server {
    pub fn start(response: Response<String>) -> Self {
        Self::delayed(response, Duration::ZERO)
    }

    pub fn delayed(response: Response<String>, delay: Duration) -> Self {
        let directory = tempfile::Builder::new().prefix("http-").tempdir().unwrap();
        let listener = UnixListener::bind(directory.path().join("s")).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let (stop, mut stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    result = &mut stopped => { result.unwrap(); break; },
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.unwrap();
                        let recorded = recorded.clone();
                        let response = response.clone();
                        connections.spawn(async move {
                            let service = hyper::service::service_fn(move |request: Request<hyper::body::Incoming>| {
                                let recorded = recorded.clone();
                                let response = response.clone();
                                async move {
                                    let (parts, body) = request.into_parts();
                                    let bytes = body.collect().await.unwrap().to_bytes();
                                    recorded.lock().unwrap().push(Request::from_parts(parts, bytes));
                                    tokio::time::sleep(delay).await;
                                    Ok::<_, Infallible>(response.map(|body| Full::new(Bytes::from(body))))
                                }
                            });
                            hyper::server::conn::http1::Builder::new()
                                .keep_alive(false)
                                .serve_connection(TokioIo::new(stream), service)
                                .await
                                .unwrap();
                        });
                    }
                }
            }
            while let Some(result) = connections.join_next().await {
                result.unwrap();
            }
        });
        Self {
            directory,
            requests,
            stop: Some(stop),
            task: Some(task),
        }
    }

    pub fn client(&self, key: Option<ApiKey>) -> OpenAiCompatClient {
        let mut client = OpenAiCompatClient::new(Url::parse("http://localhost/v1").unwrap(), key);
        client.http = reqwest::Client::builder()
            .no_proxy()
            .unix_socket(self.directory.path().join("s"))
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        client
    }

    pub async fn finish(mut self, expected: usize) -> Vec<Request<Bytes>> {
        self.stop.take().unwrap().send(()).unwrap();
        self.task.as_mut().unwrap().await.unwrap();
        self.task = None;
        let requests = std::mem::take(&mut *self.requests.lock().unwrap());
        assert_eq!(requests.len(), expected);
        for request in &requests {
            assert_eq!(request.method(), "POST");
            assert_eq!(request.uri().path(), "/v1/chat/completions");
            assert_eq!(request.headers()["content-type"], "application/json");
            assert_eq!(request.headers()["authorization"], "Bearer sk-test");
        }
        requests
    }
}

#[tokio::test]
async fn unix_http_fixture_preserves_bytes_and_removes_its_endpoint() {
    let mut response = reply(418, "fixture response");
    response
        .headers_mut()
        .insert("x-fixture", "owned".parse().unwrap());
    let server = Server::start(response);
    let socket = server.directory.path().join("s");
    let client = server
        .client(Some(ApiKey::new("sk-test".to_owned()).unwrap()))
        .http;
    let request = || {
        client
            .post("http://localhost/v1/chat/completions")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .body("{\"fixture\":true}")
    };
    let response = request().send().await.unwrap();
    assert_eq!(response.status(), 418);
    assert_eq!(response.headers()["x-fixture"], "owned");
    assert_eq!(response.text().await.unwrap(), "fixture response");
    let requests = server.finish(1).await;
    assert_eq!(requests[0].body().as_ref(), b"{\"fixture\":true}");
    assert!(!socket.exists());
    assert!(request().send().await.unwrap_err().is_connect());
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
