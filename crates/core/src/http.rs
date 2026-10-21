//! A tiny HTTP/1.1 server on loopback for the hooks and the MCP endpoint.
//!
//! Claude Code calls it for every hook, so it stays small: one thread per connection, `Connection: close`,
//! JSON in and out. The path carries a random token so other local processes cannot call it.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use serde_json::Value;

/// `(status, body)`; a body of `Null` is sent as an empty response.
pub type Handler = Arc<dyn Fn(&str, Value) -> (u16, Value) + Send + Sync>;

pub struct Server {
    port: u16,
    stop: Arc<AtomicBool>,
}

impl Server {
    /// `handler` gets the request path (without the leading slash) and the parsed JSON body.
    pub fn start(handler: Handler) -> Result<Server> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        std::thread::Builder::new().name("hook-server".into()).spawn(move || {
            for stream in listener.incoming() {
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(stream) = stream {
                    let handler = handler.clone();
                    std::thread::spawn(move || serve(stream, handler));
                }
            }
        })?;
        Ok(Server { port, stop })
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so it sees the flag.
        let _ = TcpStream::connect(SocketAddr::from((Ipv4Addr::LOCALHOST, self.port)));
    }
}

fn serve(mut stream: TcpStream, handler: Handler) {
    let Some((path, body)) = read_request(&mut stream) else { return };
    let (status, reply) = handler(&path, body);
    let text = if reply.is_null() { String::new() } else { reply.to_string() };
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Error",
    };
    let _ = write!(stream, "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len());
}

fn read_request(stream: &mut TcpStream) -> Option<(String, Value)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let (head_end, length) = loop {
        let n = stream.read(&mut chunk).ok().filter(|n| *n > 0)?;
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]).to_lowercase();
            let length = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
            break (pos + 4, length);
        }
    };
    while buf.len() < head_end + length {
        let n = stream.read(&mut chunk).ok().filter(|n| *n > 0)?;
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let path = head.split_whitespace().nth(1)?.trim_start_matches('/').to_string();
    let body = serde_json::from_slice(&buf[head_end..head_end + length]).unwrap_or(Value::Null);
    Some((path, body))
}
