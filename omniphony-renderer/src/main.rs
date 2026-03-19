#![allow(dead_code)]

use anyhow::Result;
use clap::Parser as ClapParser;
use cli::command::{Cli, Commands, LogFormat, LogLevel};
use cli::decode::cmd_render;
#[cfg(feature = "saf_vbap")]
use cli::generate_vbap::cmd_generate_vbap;
#[cfg(target_os = "windows")]
use cli::list_asio_devices::cmd_list_asio_devices;
use log::info;
use std::ffi::OsString;

mod bridge_loader;
mod cli;
mod events;
mod input;
pub(crate) mod timestamp;

fn normalize_cli_args<I>(args: I) -> Vec<OsString>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args: Vec<OsString> = args.into_iter().collect();
    if args.len() <= 1 {
        return args;
    }

    let known_subcommands = [
        OsString::from("render"),
        #[cfg(feature = "saf_vbap")]
        OsString::from("generate-vbap"),
        #[cfg(target_os = "windows")]
        OsString::from("list-asio-devices"),
        OsString::from("help"),
    ];
    let top_level_passthrough = [
        OsString::from("-h"),
        OsString::from("--help"),
        OsString::from("-V"),
        OsString::from("--version"),
    ];

    let mut insert_at = 1usize;
    while insert_at < args.len() {
        let arg = &args[insert_at];
        if top_level_passthrough.iter().any(|v| v == arg) {
            return args;
        }
        if known_subcommands.iter().any(|v| v == arg) {
            return args;
        }

        let arg_str = arg.to_string_lossy();
        if !arg_str.starts_with('-') || arg_str == "-" {
            break;
        }

        let consumes_next = matches!(arg_str.as_ref(), "--config" | "--loglevel" | "--log-format");
        insert_at += 1;
        if consumes_next && insert_at < args.len() {
            insert_at += 1;
        }
    }

    args.insert(insert_at, OsString::from("render"));
    args
}

/// Spawn a background thread that patches the DACL on JACK client named pipes
/// created by libjack during `jack_client_open`, making them world-accessible.
///
/// When orender runs as a Windows service (Session 0 / LocalSystem), libjack
/// calls `CreateNamedPipeW(NULL)` which applies a hardcoded Windows security
/// descriptor (LocalSystem=FULL, Admins=FULL, Everyone=READ).  This blocks
/// the JACK server in Session 1 from connecting (err = 5 / ACCESS_DENIED).
///
/// We cannot open the pipe as a client to patch it — that would consume
/// libjack's `ConnectNamedPipe` wait and leave jackd with no instance to
/// connect to (err = 121 / SEM_TIMEOUT).
///
/// Instead we enumerate handles in our own process via `NtQuerySystemInformation`
/// to find the server-side pipe handles libjack holds, then call
/// `SetKernelObjectSecurity` directly on those handles.  No client connection
/// is made; the pipe instance remains available for jackd.
#[cfg(windows)]
fn spawn_jack_pipe_dacl_watcher() {
    use std::ffi::c_void;
    use windows::Win32::Foundation::{BOOL, HANDLE};
    use windows::Win32::Security::{
        DACL_SECURITY_INFORMATION, InitializeSecurityDescriptor, PSECURITY_DESCRIPTOR,
        SECURITY_DESCRIPTOR, SetKernelObjectSecurity, SetSecurityDescriptorDacl,
    };
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows::Win32::System::Threading::GetCurrentProcessId;

    std::thread::Builder::new()
        .name("jack-pipe-dacl-watcher".into())
        .spawn(move || {
            // --- Resolve NT native APIs from ntdll ---
            type NtQuerySysInfo =
                unsafe extern "system" fn(u32, *mut c_void, u32, *mut u32) -> i32;
            type NtQueryObj =
                unsafe extern "system" fn(isize, u32, *mut c_void, u32, *mut u32) -> i32;

            let (nt_qsi, nt_qo): (NtQuerySysInfo, NtQueryObj) = unsafe {
                let ntdll = match GetModuleHandleW(windows::core::w!("ntdll.dll")).ok() {
                    Some(h) => h,
                    None => {
                        log::error!("jack-pipe-dacl-watcher: GetModuleHandleW(ntdll) failed");
                        return;
                    }
                };
                let qsi = GetProcAddress(ntdll, windows::core::s!("NtQuerySystemInformation"));
                let qo = GetProcAddress(ntdll, windows::core::s!("NtQueryObject"));
                match (qsi, qo) {
                    (Some(a), Some(b)) => (std::mem::transmute(a), std::mem::transmute(b)),
                    _ => {
                        log::error!("jack-pipe-dacl-watcher: NT API lookup failed");
                        return;
                    }
                }
            };

            // --- Build null-DACL security descriptor (reused for every patch) ---
            let mut sd = unsafe { std::mem::zeroed::<SECURITY_DESCRIPTOR>() };
            let psd = PSECURITY_DESCRIPTOR(&mut sd as *mut SECURITY_DESCRIPTOR as *mut _);
            if unsafe { InitializeSecurityDescriptor(psd, 1) }.is_err()
                || unsafe { SetSecurityDescriptorDacl(psd, BOOL(1), None, BOOL(0)) }.is_err()
            {
                log::error!("jack-pipe-dacl-watcher: failed to build null DACL SD");
                return;
            }

            // SYSTEM_HANDLE_TABLE_ENTRY_INFO (64-bit layout, size = 24 bytes):
            //   u16 unique_process_id    (offset  0)
            //   u16 creator_back_trace   (offset  2)
            //   u8  object_type_index    (offset  4)
            //   u8  handle_attributes    (offset  5)
            //   u16 handle_value         (offset  6)
            //   u64 object               (offset  8)  ← pointer-sized
            //   u32 granted_access       (offset 16)
            //   u32 _pad                 (offset 20)  ← alignment to 8
            #[repr(C)]
            #[derive(Copy, Clone)]
            struct HandleEntry {
                unique_process_id: u16,
                _creator_back_trace: u16,
                _object_type_index: u8,
                _handle_attributes: u8,
                handle_value: u16,
                _object: u64,
                _granted_access: u32,
                _pad: u32,
            }

            // UNICODE_STRING (64-bit): Length(u16), MaxLen(u16), pad(u32), Buffer(*u16)
            #[repr(C)]
            struct UnicodeString {
                length: u16,
                _maximum_length: u16,
                _pad: u32,
                buffer: *const u16,
            }

            // NT pipe path prefix we expect from NtQueryObject
            let prefix: Vec<u16> =
                "\\Device\\NamedPipe\\client_jack_orender_".encode_utf16().collect();

            let current_pid = unsafe { GetCurrentProcessId() } as u16;
            let mut patched = [false; 16];

            loop {
                if sys::ShutdownHandle::is_requested() {
                    break;
                }
                if patched.iter().all(|&p| p) {
                    break;
                }

                // --- Enumerate all system handles (SystemHandleInformation = 16) ---
                let mut buf: Vec<u8> = vec![0u8; 512 * 1024];
                let mut ret_len: u32 = 0;
                loop {
                    let s = unsafe {
                        nt_qsi(16, buf.as_mut_ptr() as _, buf.len() as u32, &mut ret_len)
                    };
                    if s == 0 {
                        break;
                    }
                    // STATUS_INFO_LENGTH_MISMATCH = 0xC0000004
                    if s == 0xC0000004u32 as i32 {
                        let new_len = (ret_len as usize + 65536).max(buf.len() * 2);
                        buf.resize(new_len, 0);
                    } else {
                        break; // unexpected error
                    }
                }

                let count = unsafe { *(buf.as_ptr() as *const u32) } as usize;
                // Safety: buffer is at least count * size_of::<HandleEntry>() bytes after the u32
                let entries: &[HandleEntry] = unsafe {
                    let ptr = (buf.as_ptr() as *const u32).add(1) as *const HandleEntry;
                    std::slice::from_raw_parts(ptr, count)
                };

                for entry in entries {
                    if entry.unique_process_id != current_pid {
                        continue;
                    }
                    let handle = HANDLE(entry.handle_value as isize);

                    // Query the object's name (ObjectNameInformation = 1).
                    // We pass a u16 buffer; NtQueryObject writes UNICODE_STRING then the chars.
                    let mut name_buf = vec![0u16; 512];
                    let mut nret: u32 = 0;
                    let ns = unsafe {
                        nt_qo(
                            handle.0,
                            1,
                            name_buf.as_mut_ptr() as _,
                            (name_buf.len() * 2) as u32,
                            &mut nret,
                        )
                    };
                    if ns != 0 {
                        continue;
                    }

                    let us = unsafe { &*(name_buf.as_ptr() as *const UnicodeString) };
                    if us.length == 0 || us.buffer.is_null() {
                        continue;
                    }
                    let name_chars = us.length as usize / 2;
                    let name =
                        unsafe { std::slice::from_raw_parts(us.buffer, name_chars) };

                    if name.len() <= prefix.len() || name[..prefix.len()] != *prefix {
                        continue;
                    }

                    // Parse the trailing index digit(s)
                    let suffix = String::from_utf16_lossy(&name[prefix.len()..]);
                    let idx: usize = match suffix.parse() {
                        Ok(n) if n < 16 => n,
                        _ => continue,
                    };
                    if patched[idx] {
                        continue;
                    }

                    // Patch directly on the server-side handle — no client connection.
                    let res = unsafe {
                        SetKernelObjectSecurity(handle, DACL_SECURITY_INFORMATION, psd)
                    };
                    if res.is_ok() {
                        log::info!(
                            "jack-pipe-dacl-watcher: patched handle {:#x} \
                             (client_jack_orender_{idx})",
                            entry.handle_value
                        );
                        patched[idx] = true;
                    } else {
                        log::warn!(
                            "jack-pipe-dacl-watcher: SetKernelObjectSecurity \
                             failed for orender_{idx}: {:?}",
                            res
                        );
                    }
                }

                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
        .ok();
}

/// Entry point when running as a Windows service (called by SCM via sys::windows).
/// Parses args from the service's binPath and runs the render command.
#[cfg(windows)]
fn run_as_service() -> anyhow::Result<()> {
    use clap::Parser as ClapParser;
    use cli::command::{Cli, Commands};
    use cli::decode::cmd_render;

    // Start the JACK pipe DACL watcher before entering the render loop.
    // libjack creates client pipes (\\.\pipe\client_jack_orender_N) with a
    // hardcoded restrictive DACL when called from a service; the watcher
    // patches them to null (world-accessible) so jackdmp in Session 1 can
    // connect (avoiding err = 5 / ERROR_ACCESS_DENIED).
    spawn_jack_pipe_dacl_watcher();

    let cli = Cli::parse_from(normalize_cli_args(std::env::args_os()));
    match cli.command {
        Commands::Render(ref args) => cmd_render(args, &cli),
        _ => anyhow::bail!(
            "Only the 'render' command is supported in Windows service mode. \
             Embed the full command in the service binPath, e.g.:\n  \
             sc create omniphony-renderer binPath= \"orender.exe --output-backend asio ...\""
        ),
    }
}

fn main() -> Result<()> {
    // When started by the Windows Service Control Manager, enter the SCM
    // dispatch loop and run to completion; otherwise fall through to the
    // normal console CLI flow.
    #[cfg(windows)]
    if sys::windows::try_start_service(run_as_service) {
        return Ok(());
    }

    let mut cli = Cli::parse_from(normalize_cli_args(std::env::args_os()));

    // Load global config before initializing the logger so we can apply the
    // configured log level and format.  Config errors use eprintln! directly
    // because the logger is not yet available.
    let config_path = cli
        .config
        .clone()
        .or_else(renderer::config::default_config_path);
    let global_cfg = config_path
        .as_deref()
        .map(renderer::config::Config::load_or_default)
        .unwrap_or_default()
        .global
        .unwrap_or_default();

    // Resolve effective loglevel: explicit CLI value beats config; config beats default.
    let effective_loglevel = if cli.loglevel != LogLevel::default() {
        cli.loglevel
    } else {
        global_cfg
            .loglevel
            .as_deref()
            .and_then(|s| s.parse::<LogLevel>().ok())
            .unwrap_or(cli.loglevel)
    };

    // Resolve effective log_format.
    let effective_log_format = if cli.log_format != LogFormat::default() {
        cli.log_format
    } else {
        global_cfg
            .log_format
            .as_deref()
            .and_then(|s| s.parse::<LogFormat>().ok())
            .unwrap_or(cli.log_format)
    };

    // Resolve effective strict: --strict → true, --no-strict → false, else config.
    let effective_strict = if cli.strict {
        true
    } else if cli.no_strict {
        false
    } else {
        global_cfg.strict.unwrap_or(false)
    };

    // Apply effective values back to cli so downstream code (cmd_render etc.) sees them.
    cli.loglevel = effective_loglevel;
    cli.log_format = effective_log_format;
    cli.strict = effective_strict;

    let base_level = cli.loglevel.to_level_filter();

    sys::live_log::init_logger(base_level, matches!(cli.log_format, LogFormat::Json))?;

    info!("{}", cli::command::VERSION_INFO);

    match cli.command {
        Commands::Render(ref args) => cmd_render(args, &cli)?,
        #[cfg(feature = "saf_vbap")]
        Commands::GenerateVbap(ref args) => cmd_generate_vbap(args)?,
        #[cfg(target_os = "windows")]
        Commands::ListAsioDevices => cmd_list_asio_devices()?,
    }

    Ok(())
}
