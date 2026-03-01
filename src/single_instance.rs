use std::os::unix::io::RawFd;

/// Hold this to keep the single-instance lock alive. Drops the socket on Drop.
pub struct Lock(RawFd);

impl Drop for Lock {
    fn drop(&mut self) {
        unsafe { libc::close(self.0); }
    }
}

/// Try to acquire a single-instance lock via an abstract Unix socket.
/// Returns `Some(Lock)` if we are the only instance, `None` if another is running.
/// The socket is created with SOCK_CLOEXEC so child processes never inherit it.
pub fn acquire(name: &str) -> Option<Lock> {
    unsafe {
        let sock = libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            0,
        );
        if sock < 0 {
            panic!("Failed to create single-instance socket: {}", std::io::Error::last_os_error());
        }

        // Abstract socket: sun_path starts with \0 followed by the name
        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        // addr.sun_path[0] is already 0 from zeroed() (abstract namespace marker)
        let name_bytes = name.as_bytes();
        let max_len = addr.sun_path.len() - 1; // reserve byte 0 for \0
        let copy_len = name_bytes.len().min(max_len);
        for i in 0..copy_len {
            addr.sun_path[i + 1] = name_bytes[i] as libc::c_char;
        }

        let addr_len = std::mem::size_of::<libc::sa_family_t>() + 1 + copy_len;

        let ret = libc::bind(
            sock,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            addr_len as libc::socklen_t,
        );

        if ret == 0 {
            Some(Lock(sock))
        } else {
            let err = *libc::__errno_location();
            libc::close(sock);
            if err == libc::EADDRINUSE {
                None
            } else {
                panic!("Failed to bind single-instance socket: {}", std::io::Error::from_raw_os_error(err));
            }
        }
    }
}
