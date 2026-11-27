use crate::status::App;
use crate::ws;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

const PAGE: &str = include_str!("index.html");
const MAX_BODY: usize = 1 << 20;

pub fn serve(addr: &str, app: Arc<App>) -> crate::Result<()> {
    let listener = TcpListener::bind(addr).map_err(|e| format!("can't listen on {addr}: {e}"))?;
    let local = is_loopback(addr);
    let has_password = password(&app).is_some();
    eprintln!("status page on http://{addr}{}", if has_password { " (password protected)" } else { "" });
    if !has_password && !local {
        eprintln!("warning: the status page has no password and may be reachable from your network");
        eprintln!("         editing from the page is turned off until you set [web] password");
    }
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let app = app.clone();
            std::thread::spawn(move || {
                let _ = handle(stream, &app, local);
            });
        }
    });
    Ok(())
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
}

fn read_request(stream: &TcpStream) -> std::io::Result<Request> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    let mut headers = Vec::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? <= 2 {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    let len = headers.iter().find(|(n, _)| n == "content-length").and_then(|(_, v)| v.parse().ok());
    let mut body = vec![0; len.unwrap_or(0).min(MAX_BODY)];
    reader.read_exact(&mut body)?;
    Ok(Request { method, path, headers, body })
}

fn handle(mut stream: TcpStream, app: &App, local: bool) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let req = read_request(&stream)?;
    let password = password(app);

    if let Some(pw) = &password
        && !authorized(req.header("authorization"), pw)
    {
        return write!(
            stream,
            "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"Transcodarr\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
    }
    let can_edit = password.is_some() || local;

    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/ws") if same_origin(&req) => match req.header("sec-websocket-key") {
            Some(key) => stream_status(stream, key, app, can_edit),
            None => respond(&mut stream, "400 Bad Request", "not a websocket request"),
        },
        ("GET", "/") => respond_as(&mut stream, "200 OK", "text/html; charset=utf-8", PAGE),
        ("GET", "/api/status") => {
            respond_as(&mut stream, "200 OK", "application/json", &status_json(app, can_edit))
        }
        (_, "/api/config" | "/api/reload" | "/api/retry") if !can_edit => {
            respond(&mut stream, "403 Forbidden", "set [web] password to edit from the network")
        }
        (_, "/api/config" | "/api/reload" | "/api/retry") if !trusted(&req) => {
            respond(&mut stream, "403 Forbidden", "request didn't come from the Transcodarr page")
        }
        ("GET", "/api/config") => match std::fs::read_to_string(&app.config_path) {
            Ok(text) => respond(&mut stream, "200 OK", &text),
            Err(e) => respond(&mut stream, "500 Internal Server Error", &e.to_string()),
        },
        ("POST", "/api/config") => {
            let result = app.save(&String::from_utf8_lossy(&req.body));
            reply(&mut stream, result, "saved and reloaded")
        }
        ("POST", "/api/reload") => reply(&mut stream, app.reload(), "reloaded"),
        ("POST", "/api/retry") => {
            let id = String::from_utf8_lossy(&req.body).trim().parse().unwrap_or(0);
            reply(&mut stream, app.retry(id), "queued again")
        }
        _ => respond(&mut stream, "404 Not Found", "not found"),
    }
}

fn reply(stream: &mut TcpStream, result: Result<(), String>, ok: &str) -> std::io::Result<()> {
    match result {
        Ok(()) => respond(stream, "200 OK", ok),
        Err(e) => respond(stream, "400 Bad Request", &e),
    }
}

fn respond(stream: &mut TcpStream, code: &str, text: &str) -> std::io::Result<()> {
    respond_as(stream, code, "text/plain; charset=utf-8", text)
}

fn respond_as(stream: &mut TcpStream, code: &str, kind: &str, body: &str) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {code}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn stream_status(mut stream: TcpStream, key: &str, app: &App, can_edit: bool) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        ws::accept_key(key)
    )?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut last = String::new();
    let mut idle = 0;
    loop {
        let json = status_json(app, can_edit);
        if json != last || idle >= 30 {
            ws::send_text(&mut stream, &json)?;
            last = json;
            idle = 0;
        }
        idle += 1;
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn status_json(app: &App, can_edit: bool) -> String {
    let mut v = serde_json::to_value(&*app.status.lock().unwrap()).unwrap_or_default();
    v["can_edit"] = can_edit.into();
    v.to_string()
}

fn password(app: &App) -> Option<String> {
    app.config().web.as_ref().and_then(|w| w.password.clone())
}

fn authorized(header: Option<&str>, password: &str) -> bool {
    let Some(encoded) = header.and_then(|h| h.strip_prefix("Basic ")) else { return false };
    let decoded = ws::base64_decode(encoded).unwrap_or_default();
    String::from_utf8_lossy(&decoded).split_once(':').is_some_and(|(_, pw)| pw == password)
}

fn trusted(req: &Request) -> bool {
    req.header("x-transcodarr") == Some("1") && req.header("host").is_some_and(host_is_ip) && same_origin(req)
}

fn same_origin(req: &Request) -> bool {
    match (req.header("origin"), req.header("host")) {
        (Some(origin), Some(host)) => origin.split_once("://").is_some_and(|(_, o)| o == host),
        (Some(_), None) => false,
        (None, _) => true,
    }
}

fn host_is_ip(host: &str) -> bool {
    if host.starts_with('[') {
        return true;
    }
    let name = host.rsplit_once(':').map_or(host, |(h, _)| h);
    name == "localhost" || name.parse::<Ipv4Addr>().is_ok()
}

fn is_loopback(addr: &str) -> bool {
    addr.starts_with("127.") || addr.starts_with("localhost") || addr.starts_with("[::1]")
}

#[cfg(test)]
mod tests {
    #[test]
    fn host_is_ip() {
        use super::host_is_ip;
        assert!(host_is_ip("127.0.0.1:8686"));
        assert!(host_is_ip("192.168.1.20:8686"));
        assert!(host_is_ip("localhost:8686"));
        assert!(host_is_ip("[::1]:8686"));
        assert!(!host_is_ip("evil.example.com:8686"));
    }
}
