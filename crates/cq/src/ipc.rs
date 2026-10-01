//! Local IPC transport: named pipes on Windows, unix domain sockets elsewhere.
//!
//! Endpoints are restricted to the current user because anything that can connect can type into
//! the agent's terminal.

use anyhow::Result;
use tokio::io::{AsyncRead, AsyncWrite};

/// A bidirectional local IPC stream.
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> Stream for T {}

pub type BoxStream = Box<dyn Stream>;

#[cfg(windows)]
mod imp {
    use super::BoxStream;
    use anyhow::{Context, Result, bail};
    use std::ffi::c_void;
    use std::time::Duration;
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, HANDLE, LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// Security descriptor granting access only to the current user.
    struct UserOnlySd(PSECURITY_DESCRIPTOR);

    // SAFETY: the descriptor is immutable after creation and freed only on drop.
    unsafe impl Send for UserOnlySd {}
    unsafe impl Sync for UserOnlySd {}

    impl Drop for UserOnlySd {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0) };
        }
    }

    fn current_user_sid() -> Result<String> {
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                bail!("OpenProcessToken failed: {}", std::io::Error::last_os_error());
            }
            let mut len = 0u32;
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
            let mut buf = vec![0u8; len as usize];
            let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len);
            CloseHandle(token);
            if ok == 0 {
                bail!("GetTokenInformation failed: {}", std::io::Error::last_os_error());
            }
            let user = &*(buf.as_ptr() as *const TOKEN_USER);
            let mut s: *mut u16 = std::ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut s) == 0 {
                bail!("ConvertSidToStringSidW failed: {}", std::io::Error::last_os_error());
            }
            let mut n = 0;
            while *s.add(n) != 0 {
                n += 1;
            }
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(s, n));
            LocalFree(s.cast());
            Ok(sid)
        }
    }

    fn user_only_sd() -> Result<UserOnlySd> {
        // Protected DACL with a single ACE: generic all for the current user.
        let sddl = format!("D:P(A;;GA;;;{})", current_user_sid()?);
        let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            bail!("invalid security descriptor: {}", std::io::Error::last_os_error());
        }
        Ok(UserOnlySd(sd))
    }

    /// Listening endpoint that yields one stream per client.
    pub struct Listener {
        name: String,
        sd: UserOnlySd,
        next: NamedPipeServer,
    }

    impl Listener {
        fn create(name: &str, sd: &UserOnlySd, first: bool) -> Result<NamedPipeServer> {
            let mut attrs = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd.0 as *mut c_void,
                bInheritHandle: 0,
            };
            let server = unsafe {
                ServerOptions::new()
                    .first_pipe_instance(first)
                    .reject_remote_clients(true)
                    .create_with_security_attributes_raw(name, (&raw mut attrs).cast())
            }
            .with_context(|| format!("create pipe {name}"))?;
            Ok(server)
        }

        /// Binds the endpoint. Fails if another process already owns it.
        pub fn bind(name: &str) -> Result<Self> {
            let sd = user_only_sd()?;
            let next = Self::create(name, &sd, true)?;
            Ok(Self { name: name.to_owned(), sd, next })
        }

        /// Waits for the next client.
        pub async fn accept(&mut self) -> Result<BoxStream> {
            self.next.connect().await?;
            let fresh = Self::create(&self.name, &self.sd, false)?;
            let connected = std::mem::replace(&mut self.next, fresh);
            Ok(Box::new(connected))
        }
    }

    /// Connects to an endpoint, retrying while all pipe instances are busy, up to `timeout`.
    pub async fn connect(name: &str, timeout: Duration) -> Result<BoxStream> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match ClientOptions::new().open(name) {
                Ok(c) => return Ok(Box::new(c)),
                Err(e)
                    if (e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
                        || e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32))
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => return Err(e).with_context(|| format!("connect {name}")),
            }
        }
    }

    /// Removes a stale endpoint (no-op on Windows: pipes vanish with their owner).
    pub fn cleanup(_name: &str) {}
}

#[cfg(unix)]
mod imp {
    use super::BoxStream;
    use anyhow::{Context, Result};
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;
    use tokio::net::{UnixListener, UnixStream};

    /// Listening endpoint that yields one stream per client.
    pub struct Listener(UnixListener);

    impl Listener {
        /// Binds the endpoint with mode 0600.
        pub fn bind(name: &str) -> Result<Self> {
            let _ = std::fs::remove_file(name);
            let l = UnixListener::bind(name).with_context(|| format!("bind {name}"))?;
            std::fs::set_permissions(name, std::fs::Permissions::from_mode(0o600))?;
            Ok(Self(l))
        }

        /// Waits for the next client.
        pub async fn accept(&mut self) -> Result<BoxStream> {
            let (s, _) = self.0.accept().await?;
            Ok(Box::new(s))
        }
    }

    /// Connects to an endpoint, retrying until `timeout` while it does not exist yet.
    pub async fn connect(name: &str, timeout: Duration) -> Result<BoxStream> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match UnixStream::connect(name).await {
                Ok(s) => return Ok(Box::new(s)),
                Err(_) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => return Err(e).with_context(|| format!("connect {name}")),
            }
        }
    }

    /// Removes the socket file.
    pub fn cleanup(name: &str) {
        let _ = std::fs::remove_file(name);
    }
}

pub use imp::{Listener, cleanup, connect};

/// Connects without waiting; used by hooks that must never block the agent.
pub async fn connect_now(name: &str) -> Result<BoxStream> {
    connect(name, std::time::Duration::ZERO).await
}
