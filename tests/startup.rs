use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{self, BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const BINARY: &str = env!("CARGO_BIN_EXE_jxpan-rust");
const TIMEOUT: Duration = Duration::from_secs(10);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const QRCODE_SHA256: &str = "79ec86f82856005b1c887905cfccfcfbec3821ca61c7fd5a952faa5f778f791c";

struct TestDb(PathBuf);

impl TestDb {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("jxpan-startup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).expect("create isolated startup directory");
        Self(directory)
    }

    fn path(&self) -> PathBuf {
        self.0.join("state.sqlite")
    }

    fn command(&self, address: &str) -> Command {
        let mut command = Command::new(BINARY);
        // Clear every inherited credential/legacy seed, admin setting and proxy.
        // The explicit DB path and disposable cwd also guard against default DB use.
        command
            .env_clear()
            .current_dir(&self.0)
            .env("JXPAN_LISTEN_ADDR", address)
            .env("JXPAN_DB_PATH", self.path())
            .env("JXPAN_AUTH_REFRESH", "false");
        command
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        // This fixture owns only this unique temporary directory, never a service DB.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// Tests own this child; even an assertion failure must stop and reap it before DB cleanup.
struct TestServer(Child);

impl TestServer {
    fn start(db: &TestDb) -> (Self, SocketAddr) {
        let mut server = Self(
            db.command("127.0.0.1:0")
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn isolated test server"),
        );
        let stdout = server.0.stdout.take().expect("test server stdout");
        let (sender, receiver) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut line = String::new();
            let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
            let _ = sender.send(result);
        });
        let line = receiver
            .recv_timeout(TIMEOUT)
            .expect("test server startup timed out")
            .expect("read test server announcement");
        reader.join().expect("join announcement reader");
        let address: SocketAddr = line
            .trim()
            .strip_prefix("jxpan-rust listening on ")
            .expect("test server did not announce its address")
            .parse()
            .expect("parse test server address");
        assert!(address.ip().is_loopback());
        assert_ne!(address.port(), 0);
        (server, address)
    }

    fn wait_until(&mut self, timeout: Duration) -> io::Result<Option<ExitStatus>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.0.try_wait()? {
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn shutdown(&mut self) -> io::Result<(ExitStatus, bool)> {
        if let Some(status) = self.0.try_wait()? {
            return Ok((status, false));
        }
        #[cfg(unix)]
        {
            // Signal only the PID of the child this fixture spawned, not a real service.
            let _ = Command::new("/bin/kill")
                .args(["-TERM", &self.0.id().to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        #[cfg(not(unix))]
        self.0.kill()?;
        if let Some(status) = self.wait_until(SHUTDOWN_TIMEOUT)? {
            return Ok((status, false));
        }
        // SIGKILL is a last resort only after the graceful shutdown deadline.
        self.0.kill()?;
        Ok((self.0.wait()?, true))
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

struct Response {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl Response {
    fn header(&self, name: &str) -> &str {
        self.headers.get(name).map(String::as_str).unwrap_or("")
    }
}

fn get(address: SocketAddr, path: &str) -> Response {
    let mut stream = TcpStream::connect_timeout(&address, TIMEOUT).expect("connect to test server");
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(TIMEOUT)).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .expect("write test request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .expect("read test response");
    let split = response
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .expect("HTTP response header boundary");
    let mut lines = std::str::from_utf8(&response[..split])
        .expect("ASCII HTTP headers")
        .split("\r\n");
    let status = lines
        .next()
        .expect("HTTP status line")
        .split_whitespace()
        .nth(1)
        .expect("HTTP status code")
        .parse()
        .expect("numeric HTTP status");
    let headers: BTreeMap<_, _> = lines
        .map(|line| {
            let (name, value) = line.split_once(':').expect("HTTP header");
            (name.to_ascii_lowercase(), value.trim().to_owned())
        })
        .collect();
    let body = response[split + 4..].to_vec();
    // These buffered page/JSON/asset responses have a known Content-Length.
    assert!(!headers.contains_key("transfer-encoding"));
    if let Some(length) = headers.get("content-length") {
        assert_eq!(body.len(), length.parse::<usize>().unwrap());
    }
    Response {
        status,
        headers,
        body,
    }
}

fn assert_login(response: &Response) {
    assert_eq!(response.header("cache-control"), "no-store");
    assert_eq!(response.header("referrer-policy"), "same-origin");
    assert_eq!(response.header("x-frame-options"), "DENY");
    assert!(!response.headers.contains_key("access-control-allow-origin"));
}

fn assert_unmigrated(response: &Response) {
    assert_eq!(response.status, 200, "legacy parse failure keeps HTTP 200");
    assert_eq!(
        response.header("content-type"),
        "application/json; charset=utf-8"
    );
    assert!(!response.headers.contains_key("location"));
    let value: serde_json::Value =
        serde_json::from_slice(&response.body).expect("unmigrated JSON response");
    assert_eq!(value["code"], 501);
    assert_eq!(value["success"], false, "an unmigrated API is not success");
    assert_eq!(value["msg"], "该网盘尚未迁移");
    assert!(value["data"].is_null());
}

#[test]
fn invalid_configuration_exits_without_starting_a_server() {
    let db = TestDb::new();
    let mut server = TestServer(
        db.command("invalid")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn invalid configuration test"),
    );
    let status = server
        .wait_until(TIMEOUT)
        .expect("wait for invalid configuration")
        .expect("invalid configuration did not exit");
    assert!(!status.success());
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    server
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    server
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut stderr)
        .unwrap();
    assert!(stdout.is_empty());
    assert!(!stderr.is_empty());
    assert!(
        !db.path().exists(),
        "invalid listen address must not open a DB"
    );
}

#[test]
fn serves_current_patched_pages_and_does_not_fake_unmigrated_apis() {
    let db = TestDb::new();
    let (_server, address) = TestServer::start(&db);
    for (path, page) in [
        ("/", include_bytes!("../web/index.html").as_slice()),
        (
            "/admin",
            include_bytes!("../web/admin-login.html").as_slice(),
        ),
        (
            "/admin/",
            include_bytes!("../web/admin-login.html").as_slice(),
        ),
    ] {
        let response = get(address, path);
        assert_eq!(response.status, 200, "{path}: unexpected status");
        assert_eq!(response.header("content-type"), "text/html; charset=utf-8");
        assert!(response.body == page, "{path}: not exact current web bytes");
        if path.starts_with("/admin") {
            assert_login(&response);
            assert!(!response.headers.contains_key("set-cookie"));
        }
    }
    assert!(include_str!("../web/index.html").contains("src=\"/assets/qrcode.js\""));
    assert!(include_str!("../web/admin.html").contains("src=\"/assets/qrcode.js\""));
    let asset = get(address, "/assets/qrcode.js");
    assert_eq!(asset.status, 200);
    assert_eq!(
        asset.header("content-type"),
        "application/javascript; charset=utf-8"
    );
    assert_eq!(asset.header("x-content-type-options"), "nosniff");
    assert_eq!(asset.header("cache-control"), "public, max-age=3600");
    assert!(asset.body == include_bytes!("../web/vendor/qrcode.js"));
    assert_eq!(format!("{:x}", Sha256::digest(&asset.body)), QRCODE_SHA256);
    assert_unmigrated(&get(address, "/?url=example&type=down"));
    assert_eq!(get(address, "/api/create-short").status, 405);
    assert!(db.path().is_file(), "server must use its isolated database");
}

#[cfg(unix)]
#[test]
fn owned_child_sigterm_exits_normally_and_database_reopens_without_loss() {
    use rusqlite::{Connection, OpenFlags};
    use std::os::unix::process::ExitStatusExt;

    let db = TestDb::new();
    {
        let connection = Connection::open(db.path()).expect("open isolated marker database");
        connection
            .execute_batch(
                "CREATE TABLE startup_marker(value TEXT NOT NULL);
                 INSERT INTO startup_marker(value) VALUES('synthetic-durable-marker');",
            )
            .expect("commit synthetic durable marker");
    }
    let (mut server, address) = TestServer::start(&db);
    assert_unmigrated(&get(address, "/?url=example&type=down"));
    let (status, forced) = server.shutdown().expect("reap owned SIGTERM child");
    assert!(
        !forced,
        "owned child exceeded the graceful shutdown deadline"
    );
    assert_eq!(
        status.signal(),
        None,
        "SIGTERM must not cause signal failure"
    );
    assert!(status.success(), "owned child did not exit normally");

    // A narrow shutdown regression: normal exit plus intact, committed SQLite work.
    // This does not claim to exercise draining a live upstream/provider request.
    let connection = Connection::open_with_flags(db.path(), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("reopen isolated database read-only");
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("check reopened database integrity");
    assert_eq!(integrity, "ok");
    let marker: String = connection
        .query_row("SELECT value FROM startup_marker", [], |row| row.get(0))
        .expect("read committed marker");
    assert_eq!(marker, "synthetic-durable-marker");
    let records: i64 = connection
        .query_row("SELECT COUNT(*) FROM parse_records", [], |row| row.get(0))
        .expect("read committed parse history");
    assert_eq!(
        records, 1,
        "acknowledged parse history must survive shutdown"
    );
}
