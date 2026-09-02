use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex as StdMutex};
use std::thread;

use super::*;

#[derive(Clone)]
struct ScriptedServer {
    base_url: String,
    requests: Arc<StdMutex<Vec<CapturedRequest>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CapturedRequest {
    request_line: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl ScriptedServer {
    fn start(status: u16) -> Self {
        Self::start_with_headers(status, Vec::new())
    }

    fn start_with_headers(status: u16, headers: Vec<(&'static str, &'static str)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback test server");
        let base_url = format!("http://{}", listener.local_addr().expect("local address"));
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let captured = requests.clone();
        thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            if let Some(request) = read_request(&mut stream) {
                captured.lock().expect("request capture lock").push(request);
            }
            let reason = if status == 200 { "OK" } else { "Test Status" };
            let extra_headers = headers
                .into_iter()
                .map(|(name, value)| format!("{name}: {value}\r\n"))
                .collect::<String>();
            let wire = format!(
                "HTTP/1.1 {status} {reason}\r\n{extra_headers}Content-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(wire.as_bytes());
        });
        Self { base_url, requests }
    }

    fn requests(&self) -> Vec<CapturedRequest> {
        self.requests.lock().expect("request capture lock").clone()
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> Option<CapturedRequest> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            // Loopback test bodies are short enough to arrive with the
            // headers in one read; a second read would block forever once
            // the client has already sent everything.
            break;
        }
    }
    let text = String::from_utf8_lossy(&buffer).into_owned();
    let (head, body) = text.split_once("\r\n\r\n")?;
    let mut lines = head.lines();
    let request_line = lines.next()?.to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect();
    Some(CapturedRequest {
        request_line,
        headers,
        body: body.to_owned(),
    })
}

#[tokio::test]
async fn publish_rejects_empty_topic() {
    let client = NtfyHttpClient::new().expect("test client");
    let result = client.publish("", "title", "message").await;
    assert_eq!(result, Err(NtfyPublishError::EmptyTopic));
}

#[tokio::test]
async fn publish_rejects_topic_with_path_separator() {
    let client = NtfyHttpClient::new().expect("test client");
    let result = client.publish("a/b", "title", "message").await;
    assert_eq!(result, Err(NtfyPublishError::InvalidTopic));
}

#[tokio::test]
async fn publish_rejects_topic_with_query_or_fragment_syntax() {
    let client = NtfyHttpClient::new().expect("test client");
    for topic in ["topic?admin=true", "topic#fragment", "topic%2Fnested"] {
        assert_eq!(
            client.publish(topic, "title", "message").await,
            Err(NtfyPublishError::InvalidTopic)
        );
    }
}

#[tokio::test]
async fn publish_sends_expected_request_to_topic_and_succeeds() {
    let server = ScriptedServer::start(200);
    let client = NtfyHttpClient::for_server(&server.base_url).expect("test client");

    let result = client
        .publish("my-topic", "Ready!", "Readycheck launched")
        .await;

    assert_eq!(result, Ok(()));
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].request_line.starts_with("POST /my-topic"));
    assert!(
        requests[0]
            .headers
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case("Priority") && value == "urgent")
    );
    assert!(
        requests[0]
            .headers
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case("Title") && value == "Ready!")
    );
    assert_eq!(requests[0].body, "Readycheck launched");
}

#[tokio::test]
async fn publish_trims_trailing_slash_from_server() {
    let server = ScriptedServer::start(200);
    let base_with_slash = format!("{}/", server.base_url);
    let client = NtfyHttpClient::for_server(&base_with_slash).expect("test client");

    let result = client.publish("my-topic", "title", "message").await;

    assert_eq!(result, Ok(()));
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn publish_request_error_never_leaks_the_secret_topic() {
    // Bind a loopback port and drop it immediately so the connection is
    // refused: reqwest's error would normally append " for url (...)", which
    // contains the secret topic path segment.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback port");
    let base_url = format!("http://{}", listener.local_addr().expect("local address"));
    drop(listener);
    let client = NtfyHttpClient::for_server(&base_url).expect("test client");

    let topic = "cama-secret-topic-0123456789abcdef";
    let result = client.publish(topic, "title", "message").await;

    let Err(NtfyPublishError::Request(message)) = result else {
        panic!("expected a request error, got {result:?}");
    };
    assert!(
        !message.contains(topic),
        "request error leaked the topic: {message}"
    );
}

#[tokio::test]
async fn publish_maps_non_success_status_to_rejected() {
    let server = ScriptedServer::start(500);
    let client = NtfyHttpClient::for_server(&server.base_url).expect("test client");

    let result = client.publish("my-topic", "title", "message").await;

    assert_eq!(result, Err(NtfyPublishError::Rejected(500)));
}

#[tokio::test]
async fn publish_does_not_follow_redirects() {
    let server =
        ScriptedServer::start_with_headers(307, vec![("Location", "http://127.0.0.1:1/internal")]);
    let client = NtfyHttpClient::for_server(&server.base_url).expect("test client");

    let result = client.publish("my-topic", "title", "message").await;

    assert_eq!(result, Err(NtfyPublishError::Rejected(307)));
    assert_eq!(server.requests().len(), 1);
}
