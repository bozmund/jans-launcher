//! Read Modrinth's Windows registration without a locale-dependent child shell.
use anyhow::{Context, Result, ensure};
use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};
use windows_sys::Win32::{
    Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS, LocalFree},
    System::Registry::{
        HKEY, HKEY_CLASSES_ROOT, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW,
    },
    UI::Shell::CommandLineToArgvW,
};

fn registry_command(root: HKEY, key: &str) -> Result<Vec<u16>> {
    let key: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
    let mut buffer = vec![0u16; 1024];
    for _ in 0..4 {
        let mut bytes = (buffer.len() * 2) as u32;
        // RegGetValueW also expands REG_EXPAND_SZ using the current user's environment.
        let status = unsafe {
            RegGetValueW(
                root,
                key.as_ptr(),
                std::ptr::null(),
                RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if status == ERROR_MORE_DATA {
            ensure!(bytes <= 65536, "Modrinth's registered command is too long");
            buffer.resize((bytes as usize).div_ceil(2) + 1, 0);
            continue;
        }
        if status != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(status as i32))
                .context("Cannot read Modrinth's Windows registration");
        }
        ensure!(bytes % 2 == 0, "Invalid Windows registry string length");
        buffer.truncate(bytes as usize / 2);
        while buffer.last() == Some(&0) {
            buffer.pop();
        }
        ensure!(
            !buffer.is_empty() && !buffer.contains(&0),
            "Empty or invalid Modrinth command"
        );
        return Ok(buffer);
    }
    anyhow::bail!("Modrinth's Windows registration changed while reading it")
}

fn executable_from_command(command: &[u16]) -> Result<PathBuf> {
    let command: Vec<u16> = command.iter().copied().chain(Some(0)).collect();
    let mut count = 0;
    let args = unsafe { CommandLineToArgvW(command.as_ptr(), &mut count) };
    ensure!(
        !args.is_null(),
        "Cannot parse Modrinth's registered command"
    );
    let executable = unsafe {
        if count > 0 {
            let first = *args;
            let mut len = 0;
            while *first.add(len) != 0 {
                len += 1;
            }
            Some(OsString::from_wide(std::slice::from_raw_parts(first, len)))
        } else {
            None
        }
    };
    unsafe {
        LocalFree(args.cast());
    }
    let executable = executable.context("Modrinth's registered command has no executable")?;
    ensure!(
        !executable.is_empty(),
        "Modrinth's registered executable is empty"
    );
    Ok(PathBuf::from(executable))
}

pub(super) fn executable() -> Result<PathBuf> {
    executable_from_command(&registry_command(
        HKEY_CLASSES_ROOT,
        r"modrinth\shell\open\command",
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RegCloseKey, RegCreateKeyW, RegDeleteTreeW, RegSetValueExW,
    };

    #[test]
    fn windows_registration_preserves_unicode_and_spaces() {
        let path = r"C:\Users\Jan Božac šđčćž 🐾\AppData\Local\Modrinth App\Modrinth App.exe";
        let command = format!("\"{path}\" \"%1\"");
        let wide: Vec<u16> = command.encode_utf16().collect();
        assert_eq!(executable_from_command(&wide).unwrap(), PathBuf::from(path));
    }

    #[test]
    fn native_registry_roundtrip_does_not_use_powershell_codepage() {
        // Isolated per-run HKCU fixture; never changes Modrinth's registration.
        let key = format!(
            r"Software\JansLauncher\TestFixtures\{}-{}",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        );
        let key_w: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
        let mut handle = std::ptr::null_mut();
        assert_eq!(
            unsafe { RegCreateKeyW(HKEY_CURRENT_USER, key_w.as_ptr(), &mut handle) },
            ERROR_SUCCESS
        );
        let path = r"C:\Users\Jan Božac\Modrinth App\Modrinth App.exe";
        let value: Vec<u16> = format!("\"{path}\" \"%1\"")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let status = unsafe {
            RegSetValueExW(
                handle,
                std::ptr::null(),
                0,
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * 2) as u32,
            )
        };
        unsafe {
            RegCloseKey(handle);
        }
        let read =
            registry_command(HKEY_CURRENT_USER, &key).and_then(|s| executable_from_command(&s));
        let deleted = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, key_w.as_ptr()) };
        assert_eq!(status, ERROR_SUCCESS);
        assert_eq!(deleted, ERROR_SUCCESS);
        assert_eq!(read.unwrap(), PathBuf::from(path));
    }
}
