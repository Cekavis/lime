use lime_core::service::framing::{read_json, write_json};
use lime_protocol::{Request, Response};
use std::{
    env,
    io::{self, Read, Write},
    process::Command,
    thread,
    time::Duration,
};

const MAX_CONNECT_ATTEMPTS: usize = 20;

enum Transport {
    File(std::fs::File),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
}

impl Read for Transport {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::File(file) => file.read(buffer),
            #[cfg(unix)]
            Self::Unix(stream) => stream.read(buffer),
        }
    }
}

impl Write for Transport {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            Self::File(file) => file.write(buffer),
            #[cfg(unix)]
            Self::Unix(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::File(file) => file.flush(),
            #[cfg(unix)]
            Self::Unix(stream) => stream.flush(),
        }
    }
}

fn connect_once() -> io::Result<Transport> {
    #[cfg(windows)]
    {
        let pipe = env::var("LIME_PIPE").unwrap_or_else(|_| r"\\.\pipe\lime-core-v1".to_owned());
        return std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(pipe)
            .map(Transport::File);
    }

    #[cfg(unix)]
    {
        let socket = env::var_os("LIME_SOCKET")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp/lime-core.sock"));
        return std::os::unix::net::UnixStream::connect(socket).map(Transport::Unix);
    }

    #[allow(unreachable_code)]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Lime IPC is unsupported on this platform",
    ))
}

fn connect_service() -> Result<Transport, String> {
    if let Ok(stream) = connect_once() {
        return Ok(stream);
    }

    if let Some(path) = env::var_os("LIME_SERVICE_PATH") {
        Command::new(path)
            .spawn()
            .map_err(|error| format!("unable to start Lime service: {error}"))?;
        for _ in 0..MAX_CONNECT_ATTEMPTS {
            thread::sleep(Duration::from_millis(50));
            if let Ok(stream) = connect_once() {
                return Ok(stream);
            }
        }
    }

    Err("Lime service is unavailable".to_owned())
}

fn protocol_error(response: Response) -> Result<Response, String> {
    match response {
        Response::Error { code } => Err(format!("{} ({})", code.name(), code.code())),
        response => Ok(response),
    }
}

pub fn call(request: Request) -> Result<Response, String> {
    let mut stream = connect_service()?;
    write_json(&mut stream, &Request::Handshake(Default::default()))
        .map_err(|error| format!("IPC handshake write failed: {error}"))?;
    let handshake: Response = read_json(&mut stream)
        .map_err(|error| format!("IPC handshake read failed: {error}"))?;
    match handshake {
        Response::Handshake(value) if value.accepted => {}
        Response::Handshake(value) => {
            let detail = value
                .error
                .map(|error| format!("{} ({})", error.name(), error.code()))
                .unwrap_or_else(|| "protocol version mismatch".to_owned());
            return Err(format!("Lime service rejected the connection: {detail}"));
        }
        _ => return Err("Lime service returned an invalid handshake response".to_owned()),
    }

    write_json(&mut stream, &request).map_err(|error| format!("IPC request write failed: {error}"))?;
    let response: Response = read_json(&mut stream)
        .map_err(|error| format!("IPC response read failed: {error}"))?;
    protocol_error(response)
}
