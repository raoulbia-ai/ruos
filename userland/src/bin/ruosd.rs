//! ruosd — the ruOS AI system service.
//!
//! Listens on a Unix socket (default /run/ruosd.sock, mode 0600) and serves
//! the `proto` protocol. Each connection gets its own conversation.

use ruos::agent::{Session, SessionIo};
use ruos::config::Config;
use ruos::proto::{self, Event, Request};
use ruos::provider::{self, Provider};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;

struct Conn {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Conn {
    fn read_request(&mut self) -> io::Result<Option<Request>> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        serde_json::from_str(line.trim())
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad request: {e}")))
    }
}

impl SessionIo for Conn {
    fn emit(&mut self, event: Event) -> io::Result<()> {
        self.writer.write_all(proto::encode(&event).as_bytes())
    }

    fn confirm(&mut self, command: &str, reason: &str) -> io::Result<bool> {
        self.emit(Event::Confirm { command: command.into(), reason: reason.into() })?;
        match self.read_request()? {
            Some(Request::Confirm { approve }) => Ok(approve),
            // Anything other than an explicit yes counts as no.
            _ => Ok(false),
        }
    }
}

fn serve(stream: UnixStream, provider: Arc<dyn Provider>) -> io::Result<()> {
    let mut conn = Conn { reader: BufReader::new(stream.try_clone()?), writer: stream };
    let mut session = Session::default();
    while let Some(req) = conn.read_request()? {
        match req {
            Request::Ask { prompt, cwd } => {
                let cwd = cwd.map(PathBuf::from).filter(|p| p.is_dir()).unwrap_or_else(|| PathBuf::from("/"));
                session.ask(provider.as_ref(), &prompt, &cwd, &mut conn)?;
            }
            Request::Reset => session = Session::default(),
            Request::Confirm { .. } => {}
        }
        conn.emit(Event::Done)?;
    }
    Ok(())
}

fn main() {
    let cfg = Config::load();
    let provider: Arc<dyn Provider> = Arc::from(provider::from_config(&cfg));
    let path = proto::socket_path();

    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("ruosd: cannot bind {path}: {e}");
            std::process::exit(1);
        }
    };
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    eprintln!("ruosd: listening on {path}, provider: {}", provider.name());

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let provider = provider.clone();
                std::thread::spawn(move || {
                    if let Err(e) = serve(stream, provider) {
                        eprintln!("ruosd: connection ended: {e}");
                    }
                });
            }
            Err(e) => eprintln!("ruosd: accept failed: {e}"),
        }
    }
}
