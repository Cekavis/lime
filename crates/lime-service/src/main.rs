use lime_core::CoreService;
use lime_ipc::{read_json, write_json};
use lime_protocol::{Request, Response};
use std::{env, io, path::PathBuf, sync::Arc};

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

fn data_dir() -> Option<PathBuf> {
    if let Some(path) = env::var_os("LIME_DATA_DIR") {
        return Some(PathBuf::from(path));
    }

    #[cfg(windows)]
    {
        return env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|path| path.join("Lime"));
    }

    #[cfg(unix)]
    {
        if let Some(path) = env::var_os("XDG_DATA_HOME") {
            return Some(PathBuf::from(path).join("lime"));
        }
        return env::var_os("HOME")
            .map(PathBuf::from)
            .map(|path| path.join(".local").join("share").join("lime"));
    }

    #[allow(unreachable_code)]
    None
}

fn rime_dir() -> Option<PathBuf> {
    if let Some(path) = env::var_os("LIME_RIME_DIR") {
        return Some(PathBuf::from(path));
    }
    let executable = env::current_exe().ok()?;
    let path = executable.parent()?.join("rime");
    path.is_dir().then_some(path)
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
    let service = Arc::new(CoreService::new_with_rime_dir(data_dir(), rime_dir()));
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
    let service = Arc::new(CoreService::new_with_rime_dir(data_dir(), rime_dir()));
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
        let security = PipeSecurity::for_current_user()?;
        let mut attributes = SecurityAttributes {
            length: std::mem::size_of::<SecurityAttributes>() as u32,
            descriptor: security.descriptor,
            inherit_handle: 0,
        };
        let handle = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                1024 * 1024,
                1024 * 1024,
                0,
                &mut attributes as *mut _ as *mut std::ffi::c_void,
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
#[repr(C)]
struct SecurityAttributes {
    length: u32,
    descriptor: *mut std::ffi::c_void,
    inherit_handle: i32,
}

#[cfg(windows)]
#[repr(C)]
struct SidAndAttributes {
    sid: *mut std::ffi::c_void,
    attributes: u32,
}

#[cfg(windows)]
#[repr(C)]
struct TokenUser {
    user: SidAndAttributes,
}

#[cfg(windows)]
struct PipeSecurity {
    descriptor: *mut std::ffi::c_void,
}

#[cfg(windows)]
impl PipeSecurity {
    fn for_current_user() -> io::Result<Self> {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let mut token = std::ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut required = 0u32;
        unsafe {
            GetTokenInformation(
                token,
                TOKEN_USER_CLASS,
                std::ptr::null_mut(),
                0,
                &mut required,
            );
        }
        if required == 0 {
            unsafe { CloseHandle(token) };
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0u8; required as usize];
        let ok = unsafe {
            GetTokenInformation(
                token,
                TOKEN_USER_CLASS,
                buffer.as_mut_ptr() as *mut std::ffi::c_void,
                required,
                &mut required,
            )
        };
        if ok == 0 {
            unsafe { CloseHandle(token) };
            return Err(io::Error::last_os_error());
        }
        unsafe { CloseHandle(token) };
        let token_user = unsafe { &*(buffer.as_ptr() as *const TokenUser) };
        let mut sid_text = std::ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(token_user.user.sid, &mut sid_text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let sid = unsafe {
            std::ffi::OsString::from_wide(std::slice::from_raw_parts(sid_text, wcslen(sid_text)))
        };
        unsafe {
            LocalFree(sid_text as *mut std::ffi::c_void);
        }
        let sddl = format!("D:P(A;;GA;;;{})", sid.to_string_lossy());
        let sddl_wide: Vec<u16> = std::ffi::OsStr::new(&sddl)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut descriptor = std::ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl_wide.as_ptr(),
                1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { descriptor })
    }
}

#[cfg(windows)]
impl Drop for PipeSecurity {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.descriptor);
        }
    }
}

#[cfg(windows)]
unsafe fn wcslen(mut value: *const u16) -> usize {
    let start = value;
    while *value != 0 {
        value = value.add(1);
    }
    value.offset_from(start) as usize
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
    fn GetCurrentProcess() -> *mut std::ffi::c_void;
    fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    fn CreateMutexW(
        attributes: *mut std::ffi::c_void,
        initial_owner: i32,
        name: *const u16,
    ) -> *mut std::ffi::c_void;
    fn GetLastError() -> u32;
}

#[cfg(windows)]
#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(
        process: *mut std::ffi::c_void,
        access: u32,
        token: *mut *mut std::ffi::c_void,
    ) -> i32;
    fn GetTokenInformation(
        token: *mut std::ffi::c_void,
        class: u32,
        information: *mut std::ffi::c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
    fn ConvertSidToStringSidW(sid: *mut std::ffi::c_void, string: *mut *mut u16) -> i32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        string: *const u16,
        revision: u32,
        descriptor: *mut *mut std::ffi::c_void,
        size: *mut u32,
    ) -> i32;
}

#[cfg(windows)]
const TOKEN_USER_CLASS: u32 = 1;
#[cfg(windows)]
const TOKEN_QUERY: u32 = 0x0008;

#[cfg(windows)]
const ERROR_ALREADY_EXISTS: u32 = 183;
