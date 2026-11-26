//! Windows-only transport shared by the desktop and the local CLI.
//! No TCP listener, credentials, or remote network transport.
use std::{
    ffi::{OsString, c_void},
    io,
    os::windows::{ffi::OsStringExt, io::AsRawHandle},
    path::Path,
    ptr,
    time::Duration,
};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, LocalFree},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        },
        GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::SECURITY_IDENTIFICATION,
    System::{
        Pipes::GetNamedPipeServerProcessId,
        Threading::{
            GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
    },
};

pub use crate::agent_protocol::{MAX_REQUEST, MAX_RESPONSE, read_frame, write_frame};

fn user_sid() -> io::Result<String> {
    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    unsafe {
        let mut token = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = Token(token);
        let mut len = 0;
        GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut len);
        if len == 0 || len > 65536 {
            return Err(io::Error::last_os_error());
        }
        // Ensure the returned TOKEN_USER is correctly aligned.
        let mut buf = vec![0usize; (len as usize).div_ceil(std::mem::size_of::<usize>())];
        if GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) == 0 {
            return Err(io::Error::last_os_error());
        }
        let user = &*buf.as_ptr().cast::<TOKEN_USER>();
        let mut sid = ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut sid) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut n = 0;
        while *sid.add(n) != 0 {
            n += 1;
        }
        let result = String::from_utf16(std::slice::from_raw_parts(sid, n))
            .map_err(|_| io::Error::other("Invalid user SID"));
        LocalFree(sid.cast());
        result
    }
}

pub fn pipe_name() -> io::Result<String> {
    Ok(format!(r"\\.\pipe\RTerm.Agent.v1.{}", user_sid()?))
}

pub fn listen(name: &str, first: bool) -> io::Result<NamedPipeServer> {
    // Explicit protected DACL: only the current Windows account is granted
    // access. Never inherit the broad default named-pipe ACL. Administrators
    // and a compromised same-user process are outside this security boundary.
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;{})", user_sid()?)
        .encode_utf16()
        .chain(Some(0))
        .collect();
    unsafe {
        let mut descriptor: *mut c_void = ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let result = ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .max_instances(13)
            .in_buffer_size(32768)
            .out_buffer_size(65536)
            .create_with_security_attributes_raw(
                name,
                (&mut attrs as *mut SECURITY_ATTRIBUTES).cast(),
            );
        LocalFree(descriptor);
        result
    }
}

// The client half is used by the companion binary and IPC tests, not the GUI.
#[allow(dead_code)]
fn verify_server(
    pipe: &tokio::net::windows::named_pipe::NamedPipeClient,
    expected: &Path,
) -> io::Result<()> {
    // Do not disclose commands to a process which merely created our pipe
    // name first. A process running under a compromised account can still
    // replace the trusted executable; this is not code signing or a sandbox.
    let mut pid = 0;
    let mut buf = vec![0u16; 32768];
    let mut len = buf.len() as u32;
    unsafe {
        if GetNamedPipeServerProcessId(pipe.as_raw_handle().cast(), &mut pid) == 0 {
            return Err(io::Error::last_os_error());
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let result = QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &mut len);
        let error = io::Error::last_os_error();
        CloseHandle(process);
        if result == 0 {
            return Err(error);
        }
    }
    let actual = std::fs::canonicalize(OsString::from_wide(&buf[..len as usize]))?;
    let expected = std::fs::canonicalize(expected)?;
    if !actual
        .as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&expected.as_os_str().to_string_lossy())
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Unexpected RTerm IPC server executable",
        ));
    }
    Ok(())
}

#[allow(dead_code)]
pub async fn request(
    name: &str,
    expected: &Path,
    body: &[u8],
    wait: Duration,
) -> io::Result<Vec<u8>> {
    // Identification-only QoS prevents a server from using this client as an
    // impersonation/elevation proxy. No automatic retry of dispatched calls.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(200);
    let mut pipe = loop {
        match ClientOptions::new()
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(name)
        {
            Ok(pipe) => break pipe,
            // Only retry opening before a single byte has been sent.
            Err(e) if e.raw_os_error() == Some(231) && tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(2)).await
            }
            Err(e) => return Err(e),
        }
    };
    verify_server(&pipe, expected)?;
    tokio::time::timeout(
        Duration::from_secs(3),
        write_frame(&mut pipe, body, MAX_REQUEST),
    )
    .await??;
    tokio::time::timeout(wait, read_frame(&mut pipe, MAX_RESPONSE)).await?
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Security::{
        Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SE_FILE_OBJECT,
        },
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl, SE_DACL_PROTECTED,
    };

    #[test]
    fn protected_pipe_acl_and_first_instance() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let name = format!("{}.acl-test.{}", pipe_name().unwrap(), std::process::id());
            let pipe = listen(&name, true).unwrap();
            assert!(
                listen(&name, true).is_err(),
                "duplicate first instance must fail"
            );
            unsafe {
                let mut descriptor = ptr::null_mut();
                let code = GetSecurityInfo(
                    pipe.as_raw_handle().cast(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &mut descriptor,
                );
                assert_eq!(code, 0);
                let mut control = 0;
                let mut revision = 0;
                assert_ne!(
                    GetSecurityDescriptorControl(descriptor, &mut control, &mut revision),
                    0
                );
                assert_ne!(control & SE_DACL_PROTECTED, 0);
                let mut sddl = ptr::null_mut();
                let mut len = 0;
                assert_ne!(
                    ConvertSecurityDescriptorToStringSecurityDescriptorW(
                        descriptor,
                        1,
                        DACL_SECURITY_INFORMATION,
                        &mut sddl,
                        &mut len
                    ),
                    0
                );
                let value =
                    String::from_utf16_lossy(std::slice::from_raw_parts(sddl, len as usize))
                        .trim_end_matches('\0')
                        .to_owned();
                LocalFree(sddl.cast());
                LocalFree(descriptor);
                assert!(
                    value.contains(&user_sid().unwrap()),
                    "pipe ACL must identify the current user"
                );
                assert_eq!(value.matches('(').count(), 1, "no additional access ACEs");
            }
        });
    }

    #[test]
    fn refuses_unexpected_pipe_server_before_transmitting() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let name = format!(
                "{}.identity-test.{}",
                pipe_name().unwrap(),
                std::process::id()
            );
            let mut pipe = listen(&name, true).unwrap();
            let expected = std::env::current_exe().unwrap().with_file_name("rterm.exe");
            let client = tokio::spawn(async move {
                request(&name, &expected, b"{}", Duration::from_secs(1)).await
            });
            pipe.connect().await.unwrap();
            assert!(client.await.unwrap().is_err());
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(500),
                    read_frame(&mut pipe, MAX_REQUEST)
                )
                .await
                .unwrap()
                .is_err()
            );
        });
    }
}
