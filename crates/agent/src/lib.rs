//! The guest agent protocol.
//!
//! The harness and the agent exchange newline-delimited JSON over a stream (vsock in VMs). Each
//! request gets exactly one response. The agent serves each connection on its own thread, so a
//! slow command doesn't block other connections.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::Command;

use serde::{Deserialize, Serialize};

/// The vsock port the agent listens on by default.
pub const DEFAULT_PORT: u32 = 7000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Ping,
    /// Run a program to completion and return its exit code and output.
    Run {
        argv: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Pong,
    /// `code` is `None` when the program was killed by a signal.
    Exited {
        code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    Error {
        message: String,
    },
}

pub fn handle(request: &Request) -> Response {
    match request {
        Request::Ping => Response::Pong,
        Request::Run { argv } => {
            let Some((program, args)) = argv.split_first() else {
                return Response::Error {
                    message: "`argv` is empty".into(),
                };
            };
            match Command::new(program).args(args).output() {
                Ok(output) => Response::Exited {
                    code: output.status.code(),
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                },
                Err(e) => Response::Error {
                    message: format!("could not start `{program}`: {e}"),
                },
            }
        }
    }
}

/// Serves requests on one connection until the peer closes it.
pub fn serve(stream: impl Read + Write) -> io::Result<()> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => handle(&request),
            Err(e) => Response::Error {
                message: format!("invalid request: {e}"),
            },
        };
        write_line(reader.get_mut(), &response)?;
    }
}

/// The harness side of a connection.
pub struct Client<S> {
    stream: BufReader<S>,
}

impl<S: Read + Write> Client<S> {
    pub fn new(stream: S) -> Self {
        Self {
            stream: BufReader::new(stream),
        }
    }

    pub fn stream(&self) -> &S {
        self.stream.get_ref()
    }

    pub fn request(&mut self, request: &Request) -> io::Result<Response> {
        write_line(self.stream.get_mut(), request)?;
        let mut line = String::new();
        if self.stream.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "agent closed the connection",
            ));
        }
        serde_json::from_str(&line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

fn write_line(writer: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use std::thread;

    #[test]
    fn requests_use_a_tagged_json_shape() {
        let run = Request::Run {
            argv: vec!["true".into()],
        };
        assert_eq!(
            serde_json::to_string(&run).unwrap(),
            r#"{"op":"run","argv":["true"]}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::Ping).unwrap(),
            r#"{"op":"ping"}"#
        );
    }

    #[test]
    fn run_reports_exit_code_and_output() {
        let response = handle(&Request::Run {
            argv: vec![
                "sh".into(),
                "-c".into(),
                "echo out; echo err >&2; exit 3".into(),
            ],
        });

        assert_eq!(
            response,
            Response::Exited {
                code: Some(3),
                stdout: "out\n".into(),
                stderr: "err\n".into(),
            }
        );
    }

    #[test]
    fn run_reports_missing_program_and_empty_argv() {
        let missing = handle(&Request::Run {
            argv: vec!["/nonexistent/program".into()],
        });
        let Response::Error { message } = missing else {
            panic!("expected an error, got {missing:?}");
        };
        assert!(
            message.starts_with("could not start `/nonexistent/program`"),
            "{message}"
        );

        assert_eq!(
            handle(&Request::Run { argv: vec![] }),
            Response::Error {
                message: "`argv` is empty".into()
            }
        );
    }

    #[test]
    fn client_and_server_talk_over_a_stream() {
        let (host, guest) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || serve(guest));

        let mut client = Client::new(host);
        assert_eq!(client.request(&Request::Ping).unwrap(), Response::Pong);
        let response = client
            .request(&Request::Run {
                argv: vec!["echo".into(), "hi".into()],
            })
            .unwrap();
        assert_eq!(
            response,
            Response::Exited {
                code: Some(0),
                stdout: "hi\n".into(),
                stderr: String::new(),
            }
        );

        drop(client);
        server.join().unwrap().unwrap();
    }

    #[test]
    fn server_answers_invalid_requests_with_an_error() {
        let (mut host, guest) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || serve(guest));

        host.write_all(b"{\"op\":\"dance\"}\n").unwrap();
        let mut line = String::new();
        BufReader::new(&host).read_line(&mut line).unwrap();
        let response: Response = serde_json::from_str(&line).unwrap();

        let Response::Error { message } = response else {
            panic!("expected an error, got {response:?}");
        };
        assert!(message.starts_with("invalid request:"), "{message}");

        drop(host);
        server.join().unwrap().unwrap();
    }
}
