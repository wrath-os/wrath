use std::process::ExitCode;
use std::thread;

use clap::Parser;
use vsock::{VMADDR_CID_ANY, VsockListener};

/// Guest agent for the wrath test harness.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// vsock port to listen on
    #[arg(long, default_value_t = wrath_agent::DEFAULT_PORT)]
    port: u32,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let listener = match VsockListener::bind_with_cid_port(VMADDR_CID_ANY, args.port) {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("error: cannot listen on vsock port {}: {e}", args.port);
            return ExitCode::FAILURE;
        }
    };
    eprintln!("listening on vsock port {}", args.port);

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                thread::spawn(move || {
                    if let Err(e) = wrath_agent::serve(stream) {
                        eprintln!("connection ended with an error: {e}");
                    }
                });
            }
            Err(e) => eprintln!("accept failed: {e}"),
        }
    }
    ExitCode::SUCCESS
}
