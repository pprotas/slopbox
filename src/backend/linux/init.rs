use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, ExitStatus};
use std::thread;

use anyhow::{Context, Result, ensure};

pub fn run(
    general_socket: Option<&Path>,
    general_port: Option<u16>,
    model_socket: Option<&Path>,
    model_port: Option<u16>,
    authenticated_http_socket: Option<&Path>,
    authenticated_http_port: Option<u16>,
    command: &[OsString],
) -> Result<ExitStatus> {
    ensure!(!command.is_empty(), "sandbox init requires a command");
    let ports: Vec<_> = [general_port, model_port, authenticated_http_port]
        .into_iter()
        .flatten()
        .collect();
    ensure!(
        ports.iter().collect::<std::collections::HashSet<_>>().len() == ports.len(),
        "sandbox proxy ports must be distinct"
    );
    match (general_socket, general_port) {
        (Some(socket), Some(port)) => spawn_forwarder("general", socket, port)?,
        (None, None) => {}
        _ => anyhow::bail!("general gateway socket and port must be provided together"),
    }
    match (model_socket, model_port) {
        (Some(socket), Some(port)) => spawn_forwarder("model", socket, port)?,
        (None, None) => {}
        _ => anyhow::bail!("model gateway socket and port must be provided together"),
    }
    match (authenticated_http_socket, authenticated_http_port) {
        (Some(socket), Some(port)) => spawn_forwarder("authenticated-http", socket, port)?,
        (None, None) => {}
        _ => anyhow::bail!("authenticated HTTP gateway socket and port must be provided together"),
    }

    Command::new(&command[0])
        .args(&command[1..])
        .status()
        .with_context(|| format!("failed to start {:?}", command[0]))
}

pub fn run_tool(
    general_socket: Option<&Path>,
    general_port: Option<u16>,
    authenticated_http_socket: Option<&Path>,
    authenticated_http_port: Option<u16>,
    command: &[OsString],
) -> Result<ExitStatus> {
    ensure!(!command.is_empty(), "tool init requires a command");
    if let (Some(general_port), Some(authenticated_http_port)) =
        (general_port, authenticated_http_port)
    {
        ensure!(
            general_port != authenticated_http_port,
            "tool proxy ports must be distinct"
        );
    }
    match (general_socket, general_port) {
        (Some(socket), Some(port)) => spawn_forwarder("general", socket, port)?,
        (None, None) => {}
        _ => anyhow::bail!("general gateway socket and port must be provided together"),
    }
    match (authenticated_http_socket, authenticated_http_port) {
        (Some(socket), Some(port)) => spawn_forwarder("authenticated-http", socket, port)?,
        (None, None) => {}
        _ => anyhow::bail!("authenticated HTTP gateway socket and port must be provided together"),
    }

    Command::new(&command[0])
        .args(&command[1..])
        .status()
        .with_context(|| format!("failed to start {:?}", command[0]))
}

fn spawn_forwarder(name: &'static str, socket: &Path, port: u16) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("failed to bind sandbox {name} gateway on port {port}"))?;
    let socket = socket.to_path_buf();
    thread::Builder::new()
        .name(format!("slopbox-{name}-forwarder"))
        .spawn(move || {
            for connection in listener.incoming() {
                match connection {
                    Ok(stream) => {
                        let socket = socket.clone();
                        thread::spawn(move || {
                            if let Err(error) = forward(stream, &socket)
                                && !expected_disconnect(&error)
                            {
                                eprintln!("slopbox {name} forwarder: {error:#}");
                            }
                        });
                    }
                    Err(error) => eprintln!("slopbox {name} forwarder: accept failed: {error}"),
                }
            }
        })?;
    Ok(())
}

fn forward(client: TcpStream, socket: &Path) -> Result<()> {
    let gateway = UnixStream::connect(socket)
        .with_context(|| format!("failed to connect to gateway socket {}", socket.display()))?;
    relay(client, gateway)
}

fn relay(mut client: TcpStream, mut gateway: UnixStream) -> Result<()> {
    let mut client_read = client.try_clone()?;
    let mut gateway_write = gateway.try_clone()?;
    let upload = thread::spawn(move || {
        let result = std::io::copy(&mut client_read, &mut gateway_write);
        let _ = gateway_write.shutdown(Shutdown::Write);
        result
    });

    std::io::copy(&mut gateway, &mut client)?;
    let _ = client.shutdown(Shutdown::Write);
    upload
        .join()
        .map_err(|_| anyhow::anyhow!("forwarder upload thread panicked"))??;
    Ok(())
}

fn expected_disconnect(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .map(|error| {
            matches!(
                error.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::UnexpectedEof
            )
        })
        .unwrap_or(false)
}

pub fn print_denials(port: u16) -> Result<()> {
    let mut stream =
        TcpStream::connect(("127.0.0.1", port)).context("Slopbox denial service is unavailable")?;
    stream.write_all(
        b"GET /denials HTTP/1.1\r\nHost: slopbox.internal\r\nConnection: close\r\n\r\n",
    )?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let (_, body) = response
        .split_once("\r\n\r\n")
        .context("invalid response from Slopbox denial service")?;
    print!("{body}");
    Ok(())
}
