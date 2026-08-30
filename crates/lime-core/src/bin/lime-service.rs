use lime_core::{
    service::framing::{read_json, write_json},
    CoreService,
};
use lime_protocol::{Request, Response};
use std::{io, sync::Arc};

fn handle_connection<S: io::Read + io::Write>(
    mut stream: S,
    service: Arc<CoreService>,
) -> io::Result<()> {
    loop {
        let request: Request = match read_json(&mut stream) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        };
        let response: Response = service.handle(request);
        write_json(&mut stream, &response)?;
    }
}

#[cfg(unix)]
fn main() -> io::Result<()> {
    use std::os::unix::net::UnixListener;
    use std::{env, path::PathBuf, sync::Arc, thread};
    let socket = env::var_os("LIME_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/lime-core.sock"));
    if socket.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("Lime service socket already exists: {}", socket.display()),
        ));
    }
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    let service = Arc::new(CoreService::new(
        env::var_os("LIME_DATA_DIR").map(PathBuf::from),
    ));
    eprintln!("lime-service listening on {}", socket.display());
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let service = Arc::clone(&service);
                thread::spawn(move || {
                    let _ = handle_connection(stream, service);
                });
            }
            Err(error) => eprintln!("IPC accept failed: {error}"),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn main() -> io::Result<()> {
    use std::{env, sync::Arc, thread};
    let _instance = SingleInstance::acquire("LimeCoreService-v1")?;
    let name = env::var("LIME_PIPE").unwrap_or_else(|_| r"\\.\pipe\lime-core-v1".to_owned());
    let service = Arc::new(CoreService::new(
        env::var_os("LIME_DATA_DIR").map(std::path::PathBuf::from),
    ));
    loop {
        let pipe = NamedPipe::create(&name)?;
        pipe.connect()?;
        let service = Arc::clone(&service);
        thread::spawn(move || {
            let _ = handle_connection(pipe, service);
        });
    }
}

#[cfg(windows)]
struct SingleInstance {
    handle: *mut std::ffi::c_void,
}

#[cfg(windows)]
impl SingleInstance {
    fn acquire(name: &str) -> io::Result<Self> {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = std::ffi::OsStr::new(name)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe { CreateMutexW(std::ptr::null_mut(), 0, wide.as_ptr()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe {
                CloseHandle(handle);
            }
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Lime service is already running",
            ));
        }
        Ok(Self { handle })
    }
}

#[cfg(windows)]
impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
struct NamedPipe {
    handle: *mut std::ffi::c_void,
}

#[cfg(windows)]
unsafe impl Send for NamedPipe {}

#[cfg(windows)]
impl NamedPipe {
    fn create(name: &str) -> io::Result<Self> {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = std::ffi::OsStr::new(name)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                1024 * 1024,
                1024 * 1024,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self { handle })
        }
    }
    fn connect(&self) -> io::Result<()> {
        let ok = unsafe { ConnectNamedPipe(self.handle, std::ptr::null_mut()) };
        if ok == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) {
                Ok(())
            } else {
                Err(error)
            }
        } else {
            Ok(())
        }
    }
}

#[cfg(windows)]
impl io::Read for NamedPipe {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut read = 0u32;
        let ok = unsafe {
            ReadFile(
                self.handle,
                buffer.as_mut_ptr() as _,
                buffer.len() as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(read as usize)
        }
    }
}
#[cfg(windows)]
impl io::Write for NamedPipe {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let mut written = 0u32;
        let ok = unsafe {
            WriteFile(
                self.handle,
                buffer.as_ptr() as _,
                buffer.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(written as usize)
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[cfg(windows)]
impl Drop for NamedPipe {
    fn drop(&mut self) {
        unsafe {
            DisconnectNamedPipe(self.handle);
            CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
const PIPE_ACCESS_DUPLEX: u32 = 0x00000003;
#[cfg(windows)]
const PIPE_TYPE_BYTE: u32 = 0x00000000;
#[cfg(windows)]
const PIPE_READMODE_BYTE: u32 = 0x00000000;
#[cfg(windows)]
const PIPE_WAIT: u32 = 0x00000000;
#[cfg(windows)]
const PIPE_UNLIMITED_INSTANCES: u32 = 255;
#[cfg(windows)]
const ERROR_PIPE_CONNECTED: u32 = 535;
#[cfg(windows)]
const INVALID_HANDLE_VALUE: *mut std::ffi::c_void = -1isize as *mut std::ffi::c_void;

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn CreateNamedPipeW(
        name: *const u16,
        open_mode: u32,
        pipe_mode: u32,
        max_instances: u32,
        out_buffer_size: u32,
        in_buffer_size: u32,
        default_timeout: u32,
        security_attributes: *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void;
    fn ConnectNamedPipe(handle: *mut std::ffi::c_void, overlapped: *mut std::ffi::c_void) -> i32;
    fn DisconnectNamedPipe(handle: *mut std::ffi::c_void) -> i32;
    fn ReadFile(
        handle: *mut std::ffi::c_void,
        buffer: *mut std::ffi::c_void,
        length: u32,
        read: *mut u32,
        overlapped: *mut std::ffi::c_void,
    ) -> i32;
    fn WriteFile(
        handle: *mut std::ffi::c_void,
        buffer: *const std::ffi::c_void,
        length: u32,
        written: *mut u32,
        overlapped: *mut std::ffi::c_void,
    ) -> i32;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    fn CreateMutexW(
        attributes: *mut std::ffi::c_void,
        initial_owner: i32,
        name: *const u16,
    ) -> *mut std::ffi::c_void;
    fn GetLastError() -> u32;
}

#[cfg(windows)]
const ERROR_ALREADY_EXISTS: u32 = 183;
