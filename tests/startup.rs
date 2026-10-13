use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

const BINARY: &str = env!("CARGO_BIN_EXE_jxpan-rust");
const TIMEOUT: Duration = Duration::from_secs(10);

// Tests own this child; never leave a server running after an assertion fails.
struct TestServer(Child);

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn invalid_configuration_exits_without_starting_a_server() {
    let output = Command::new(BINARY)
        .env("JXPAN_LISTEN_ADDR", "invalid")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}

#[test]
fn serves_exact_imported_pages_without_faking_unimplemented_apis() {
    let mut server = TestServer(
        Command::new(BINARY)
            .env("JXPAN_LISTEN_ADDR", "127.0.0.1:0")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let stdout = server.0.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        let _ = sender.send(line);
    });
    let line = receiver.recv_timeout(TIMEOUT).unwrap();
    reader.join().unwrap();
    let address: SocketAddr = line
        .trim()
        .strip_prefix("jxpan-rust listening on ")
        .unwrap()
        .parse()
        .unwrap();
    assert!(address.ip().is_loopback());
    assert_ne!(address.port(), 0);

    for (path, status, page) in [
        ("/", 200, Some(include_str!("../web/index.html"))),
        ("/admin", 200, Some(include_str!("../web/admin-login.html"))),
        ("/?url=example&type=down", 501, None),
        ("/api/create-short", 404, None),
    ] {
        let mut stream = TcpStream::connect_timeout(&address, TIMEOUT).unwrap();
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        stream.set_write_timeout(Some(TIMEOUT)).unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(
            response.starts_with(&format!("HTTP/1.1 {status}")),
            "{path}: unexpected status"
        );
        if let Some(page) = page {
            let (headers, body) = response.split_once("\r\n\r\n").unwrap();
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("content-type: text/html; charset=utf-8")
            );
            assert_eq!(body, page, "{path}: imported page changed");
        }
    }
}
