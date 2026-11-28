use super::*;
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER,
    Authorization::{ConvertStringSidToSidW, GetSecurityInfo, SE_FILE_OBJECT},
    DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetSecurityDescriptorControl,
    GetSecurityDescriptorDacl, SE_DACL_PROTECTED,
};
use windows_sys::Win32::{Foundation::GENERIC_ALL, Storage::FileSystem::FILE_ALL_ACCESS};

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

fn assert_protected_user_dacl(descriptor: &LocalAllocation, expected_sid: &str) {
    unsafe {
        let mut control = 0;
        let mut revision = 0;
        assert_ne!(
            GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision),
            0
        );
        assert_ne!(control & SE_DACL_PROTECTED, 0);
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = ptr::null_mut();
        assert_ne!(
            GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted),
            0
        );
        assert!(
            present != 0 && !dacl.is_null(),
            "DACL must be present and non-null"
        );
        assert_eq!((*dacl).AceCount, 1, "no additional access ACEs");
        let mut ace = ptr::null_mut();
        assert_ne!(GetAce(dacl, 0, &mut ace), 0);
        assert!(!ace.is_null());
        let header = &*ace.cast::<ACE_HEADER>();
        // ACCESS_ALLOWED_ACE_TYPE (0), not a deny or object-specific ACE.
        assert_eq!(header.AceType, 0);
        assert_eq!(header.AceFlags, 0);
        let ace = ace.cast::<ACCESS_ALLOWED_ACE>();
        assert!(matches!((*ace).Mask, GENERIC_ALL | FILE_ALL_ACCESS));
        let expected: Vec<u16> = expected_sid.encode_utf16().chain(Some(0)).collect();
        let mut sid = ptr::null_mut();
        assert_ne!(ConvertStringSidToSidW(expected.as_ptr(), &mut sid), 0);
        let sid = LocalAllocation(sid);
        // SDDL may abbreviate well-known accounts (e.g. SY or LA).
        // Compare actual binary SIDs instead of searching formatted text.
        assert_ne!(
            EqualSid(ptr::addr_of_mut!((*ace).SidStart).cast(), sid.0),
            0,
            "pipe ACL must identify the current user"
        );
    }
}

#[test]
fn protected_acl_accepts_well_known_sid_alias() {
    let sddl: Vec<u16> = "D:P(A;;GA;;;SY)".encode_utf16().chain(Some(0)).collect();
    let mut descriptor = ptr::null_mut();
    unsafe {
        assert_ne!(
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                ptr::null_mut()
            ),
            0
        );
    }
    assert_protected_user_dacl(&LocalAllocation(descriptor), "S-1-5-18");
}

#[test]
fn process_user_sid_matches_the_current_account() {
    assert_eq!(
        process_user_sid(unsafe { GetCurrentProcess() }).unwrap(),
        user_sid().unwrap()
    );
}

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
            assert_protected_user_dacl(&LocalAllocation(descriptor), &user_sid().unwrap());
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
        let directory = tempfile::tempdir().unwrap();
        let expected = directory.path().join("different-executable.exe");
        std::fs::write(&expected, b"not the server executable").unwrap();
        // Connect both ends before identity verification can close the client.
        let client = ClientOptions::new()
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(&name)
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), pipe.connect())
            .await
            .expect("fixture named-pipe connect timed out")
            .unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            request_connected(
                client,
                &expected,
                b"must not be sent",
                Duration::from_secs(2),
            ),
        )
        .await
        .expect("fixture client rejection timed out")
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "Unexpected RTerm IPC server executable");
        use tokio::io::AsyncReadExt;
        let mut byte = [0; 1];
        match tokio::time::timeout(Duration::from_secs(3), pipe.read(&mut byte))
            .await
            .expect("fixture peer read timed out")
        {
            Ok(0) => {}
            // ERROR_BROKEN_PIPE / ERROR_PIPE_NOT_CONNECTED / ERROR_NO_DATA.
            Err(error) if matches!(error.raw_os_error(), Some(109 | 233 | 232)) => {}
            result => panic!("request bytes reached unexpected executable: {result:?}"),
        }
    });
}
