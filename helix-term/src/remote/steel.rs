use super::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    net::{SocketAddr, TcpListener, TcpStream},
    sync::mpsc::{Receiver, RecvTimeoutError},
};

const SCHEMA_VERSION: u64 = 2;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_REQUEST_ID_BYTES: usize = 256;

pub(crate) type HandlerReceiver = Receiver<Result<Value, String>>;

#[derive(Deserialize)]
struct WireRequest {
    schema_version: u64,
    request_id: String,
    auth_token: String,
    action: String,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    payload: Value,
}

#[derive(Serialize)]
pub(crate) struct HandlerRequest {
    pub(crate) request_id: String,
    pub(crate) action: String,
    pub(crate) payload: Value,
}

#[derive(Serialize)]
struct Failure {
    class: &'static str,
    message: String,
}

impl Failure {
    fn new(class: &'static str, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
        }
    }
}

pub(crate) struct BridgeServer {
    pub(crate) addr: SocketAddr,
    server: Server<TcpStream>,
}

impl BridgeServer {
    pub(crate) fn start(
        auth_token: String,
        dispatch: impl Fn(HandlerRequest) -> HandlerReceiver + Send + 'static,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let server = Server::start(listener, move |stream, stopping| {
            serve(stream, &auth_token, &dispatch, stopping)
        })?;
        Ok(Self { addr, server })
    }

    pub(crate) fn stop(&self) {
        self.server.stop();
    }
}

fn serve(
    mut stream: impl Connection,
    auth_token: &str,
    dispatch: &impl Fn(HandlerRequest) -> HandlerReceiver,
    stopping: &AtomicBool,
) -> io::Result<()> {
    stream.set_write_timeout(Some(DEFAULT_TIMEOUT))?;

    let line = read_frame(&mut stream, DEFAULT_TIMEOUT)?;
    let bytes = line.len();
    if bytes > MAX_REQUEST_BYTES {
        return write_error(
            &mut stream,
            "",
            "request_too_large",
            "request exceeds 64 KiB",
        );
    }
    if !line.ends_with(b"\n") {
        return write_error(
            &mut stream,
            "",
            "invalid_request",
            "request must end with a newline",
        );
    }

    let request: WireRequest = match serde_json::from_slice(&line) {
        Ok(request) => request,
        Err(error) => {
            return write_error(&mut stream, "", "invalid_request", error.to_string());
        }
    };
    let request_id = request.request_id;
    if request_id.trim().is_empty() || request_id.len() > MAX_REQUEST_ID_BYTES {
        return write_error(
            &mut stream,
            "",
            "invalid_request",
            "request_id must contain 1 to 256 bytes",
        );
    }
    if request.schema_version != SCHEMA_VERSION {
        return write_error(
            &mut stream,
            &request_id,
            "unsupported_schema",
            format!("expected schema version {SCHEMA_VERSION}"),
        );
    }
    if !tokens_match(auth_token.as_bytes(), request.auth_token.as_bytes()) {
        return write_error(
            &mut stream,
            &request_id,
            "unauthorized",
            "invalid authentication token",
        );
    }
    if request.action.trim().is_empty() {
        return write_error(
            &mut stream,
            &request_id,
            "invalid_request",
            "action must be non-empty",
        );
    }

    let timeout = Duration::from_millis(
        request
            .timeout_ms
            .unwrap_or(DEFAULT_TIMEOUT.as_millis() as u64)
            .clamp(1, DEFAULT_TIMEOUT.as_millis() as u64),
    );
    let response = wait_for_handler(
        dispatch(HandlerRequest {
            request_id: request_id.clone(),
            action: request.action,
            payload: request.payload,
        }),
        timeout,
        stopping,
    );
    write_response(&mut stream, &request_id, response)
}

fn tokens_match(expected: &[u8], actual: &[u8]) -> bool {
    expected.len() == actual.len()
        && expected
            .iter()
            .zip(actual)
            .fold(0, |difference, (left, right)| difference | (left ^ right))
            == 0
}

fn wait_for_handler(
    receiver: HandlerReceiver,
    timeout: Duration,
    stopping: &AtomicBool,
) -> Result<Value, Failure> {
    let deadline = Instant::now() + timeout;
    loop {
        if stopping.load(Ordering::Acquire) {
            return Err(Failure::new("unavailable", "transport is stopping"));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Failure::new("timeout", "editor handler timed out"));
        }
        match receiver.recv_timeout(remaining.min(STOP_POLL_INTERVAL)) {
            Ok(Ok(value)) => return Ok(value),
            Ok(Err(message)) => return Err(Failure::new("handler_error", message)),
            Err(RecvTimeoutError::Disconnected) => {
                return Err(Failure::new("unavailable", "editor handler unavailable"));
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

fn write_response(
    stream: &mut impl Write,
    request_id: &str,
    result: Result<Value, Failure>,
) -> io::Result<()> {
    let response = match result {
        Ok(data) => json!({
            "schema_version": SCHEMA_VERSION,
            "request_id": request_id,
            "status": "ok",
            "data": data,
        }),
        Err(error) => json!({
            "schema_version": SCHEMA_VERSION,
            "request_id": request_id,
            "status": "error",
            "error": error,
        }),
    };
    let mut encoded = serde_json::to_vec(&response)?;
    if encoded.len() >= MAX_RESPONSE_BYTES {
        encoded = serde_json::to_vec(&json!({
            "schema_version": SCHEMA_VERSION,
            "request_id": request_id,
            "status": "error",
            "error": {
                "class": "response_too_large",
                "message": "response exceeds 64 KiB",
            },
        }))?;
    }
    encoded.push(b'\n');
    stream.write_all(&encoded)
}

fn write_error(
    stream: &mut impl Write,
    request_id: &str,
    class: &'static str,
    message: impl Into<String>,
) -> io::Result<()> {
    write_response(stream, request_id, Err(Failure::new(class, message)))
}

#[cfg(test)]
mod tests {
    use super::{
        BridgeServer, HandlerRequest, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, SCHEMA_VERSION,
    };
    use serde_json::{json, Value};
    use std::{
        io::{BufRead, BufReader, Write},
        net::{SocketAddr, TcpStream},
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc::sync_channel,
            Arc,
        },
        thread,
        time::{Duration, Instant},
    };

    fn exchange(addr: SocketAddr, request: Value) -> Value {
        exchange_raw(addr, &format!("{request}\n"))
    }

    fn exchange_raw(addr: SocketAddr, request: &str) -> Value {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();

        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        assert!(response.len() <= MAX_RESPONSE_BYTES);
        serde_json::from_str(&response).unwrap()
    }

    fn request(id: &str, token: &str, action: &str, timeout_ms: u64) -> Value {
        json!({
            "schema_version": SCHEMA_VERSION,
            "request_id": id,
            "auth_token": token,
            "action": action,
            "timeout_ms": timeout_ms,
            "payload": { "path": "/target" },
        })
    }

    #[test]
    fn loopback_transport_is_authenticated_bounded_and_stoppable() {
        let calls = Arc::new(AtomicUsize::new(0));
        let dispatch_calls = calls.clone();
        let server = BridgeServer::start("secret".into(), move |request: HandlerRequest| {
            dispatch_calls.fetch_add(1, Ordering::Relaxed);
            let (sender, receiver) = sync_channel(1);
            match request.action.as_str() {
                "timeout" => {
                    thread::spawn(move || {
                        thread::sleep(Duration::from_millis(100));
                        let _ = sender.send(Ok(Value::Null));
                    });
                }
                "large" => sender
                    .send(Ok(json!("x".repeat(MAX_RESPONSE_BYTES))))
                    .unwrap(),
                "boundary" => {
                    let overhead = serde_json::to_vec(&json!({
                        "schema_version": SCHEMA_VERSION,
                        "request_id": "boundary",
                        "status": "ok",
                        "data": "",
                    }))
                    .unwrap()
                    .len();
                    sender
                        .send(Ok(json!("x".repeat(MAX_RESPONSE_BYTES - overhead))))
                        .unwrap();
                }
                _ => sender
                    .send(Ok(json!({
                        "action": request.action,
                        "payload": request.payload,
                    })))
                    .unwrap(),
            }
            receiver
        })
        .unwrap();
        let addr = server.addr;
        assert!(addr.ip().is_loopback());
        assert_ne!(addr.port(), 0);

        let rejected = exchange(addr, request("wrong", "other", "open", 100));
        assert_eq!(rejected["status"], "error");
        assert_eq!(rejected["error"]["class"], "unauthorized");
        assert_eq!(calls.load(Ordering::Relaxed), 0);

        let malformed = exchange_raw(addr, "not JSON\n");
        assert_eq!(malformed["error"]["class"], "invalid_request");
        assert_eq!(calls.load(Ordering::Relaxed), 0);

        let oversized = exchange_raw(addr, &("x".repeat(MAX_REQUEST_BYTES + 1) + "\n"));
        assert_eq!(oversized["error"]["class"], "request_too_large");
        assert_eq!(calls.load(Ordering::Relaxed), 0);

        let accepted = exchange(addr, request("ok", "secret", "open", 100));
        assert_eq!(accepted["status"], "ok");
        assert_eq!(accepted["request_id"], "ok");
        assert_eq!(accepted["data"]["action"], "open");
        assert_eq!(accepted["data"]["payload"]["path"], "/target");

        let large = exchange(addr, request("large", "secret", "large", 100));
        assert_eq!(large["error"]["class"], "response_too_large");

        let boundary = exchange(addr, request("boundary", "secret", "boundary", 100));
        assert_eq!(boundary["error"]["class"], "response_too_large");

        let timed_out = exchange(addr, request("slow", "secret", "timeout", 10));
        assert_eq!(timed_out["error"]["class"], "timeout");
        assert_eq!(calls.load(Ordering::Relaxed), 4);

        server.stop();
        assert!(TcpStream::connect(addr).is_err());
    }

    #[test]
    fn shutdown_interrupts_an_idle_connection() {
        let server = BridgeServer::start("secret".into(), |_| sync_channel(1).1).unwrap();
        let _idle_connection = TcpStream::connect(server.addr).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while server.server.state.connection.lock().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "server did not accept connection"
            );
            thread::yield_now();
        }

        let started = Instant::now();
        drop(server);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
