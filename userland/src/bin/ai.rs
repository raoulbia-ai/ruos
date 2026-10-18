//! ai — talk to the ruOS system agent.
//!
//!   ai <request...>   one request, then exit
//!   ai                interactive; `/reset` clears the conversation, `exit` or Ctrl-D quits

use ruos::proto::{self, Event, Request};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const RESET: &str = "\x1b[0m";

struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

/// Read the approval answer from the terminal, not stdin, so piped input
/// cannot approve a command. Falls back to stdin when there is no terminal.
fn read_answer(prompt: &str) -> String {
    print!("{prompt}");
    let _ = io::stdout().flush();
    let mut line = String::new();
    match std::fs::File::open("/dev/tty") {
        Ok(tty) => {
            let _ = BufReader::new(tty).read_line(&mut line);
        }
        Err(_) => {
            let _ = io::stdin().lock().read_line(&mut line);
        }
    }
    line.trim().to_lowercase()
}

impl Client {
    fn send(&mut self, req: &Request) -> io::Result<()> {
        self.writer.write_all(proto::encode(req).as_bytes())
    }

    /// Send a request and print events until `Done`.
    fn exchange(&mut self, req: &Request) -> io::Result<()> {
        self.send(req)?;
        let mut line = String::new();
        loop {
            line.clear();
            if self.reader.read_line(&mut line)? == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "ruosd closed the connection"));
            }
            let event: Event = serde_json::from_str(line.trim())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            match event {
                Event::Text { text } => println!("{text}"),
                Event::Action { tool, summary } => println!("{DIM}· {tool} {summary}{RESET}"),
                Event::Output { text } => {
                    for l in text.lines() {
                        println!("{DIM}  │ {l}{RESET}");
                    }
                }
                Event::Confirm { command, reason } => {
                    println!("{YELLOW}{BOLD}ruosd wants to run:{RESET} {BOLD}{command}{RESET}");
                    println!("{YELLOW}  reason:{RESET} {reason}");
                    let approve = read_answer(&format!("{YELLOW}  allow? [y/N] {RESET}")) == "y";
                    self.send(&Request::Confirm { approve })?;
                }
                Event::Error { message } => println!("{RED}error:{RESET} {message}"),
                Event::Done => return Ok(()),
            }
        }
    }

    fn ask(&mut self, prompt: &str) -> io::Result<()> {
        let cwd = std::env::current_dir().ok().map(|p| p.display().to_string());
        self.exchange(&Request::Ask { prompt: prompt.to_string(), cwd })
    }
}

fn main() {
    let path = proto::socket_path();
    let stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ai: cannot reach ruosd at {path}: {e}");
            std::process::exit(1);
        }
    };
    let mut client = Client { reader: BufReader::new(stream.try_clone().expect("clone socket")), writer: stream };

    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        if let Err(e) = client.ask(&args.join(" ")) {
            eprintln!("ai: {e}");
            std::process::exit(1);
        }
        return;
    }

    println!("{DIM}ruOS agent. Type a request; /reset clears history; exit or Ctrl-D quits.{RESET}");
    loop {
        print!("{BOLD}ai>{RESET} ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        match io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => {
                println!();
                return;
            }
            Ok(_) => {}
        }
        let line = line.trim();
        let result = match line {
            "" => continue,
            "exit" | "quit" => return,
            "/reset" => client.exchange(&Request::Reset),
            prompt => client.ask(prompt),
        };
        if let Err(e) = result {
            eprintln!("ai: {e}");
            std::process::exit(1);
        }
    }
}
