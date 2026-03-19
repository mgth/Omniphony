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
/// Instead we use `NtQueryInformationProcess(ProcessHandleInformation=51)` to
/// enumerate only our own process's handles (much faster than system-wide
/// enumeration), pre-filter with `GetFileType` to avoid calling `NtQueryObject`
/// on handle types that can hang, and call `SetKernelObjectSecurity` directly
/// on the server-side pipe handle libjack holds.
#[cfg(windows)]
fn spawn_jack_pipe_dacl_watcher() {
    use std::ffi::c_void;
    use windows::Win32::Foundation::{BOOL, HANDLE};
    use windows::Win32::Security::{
        DACL_SECURITY_INFORMATION, InitializeSecurityDescriptor, PSECURITY_DESCRIPTOR,
        SECURITY_DESCRIPTOR, SetKernelObjectSecurity, SetSecurityDescriptorDacl,
    };
    use windows::Win32::Storage::FileSystem::GetFileType;
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows::Win32::System::Threading::GetCurrentProcess;

    std::thread::Builder::new()
        .name("jack-pipe-dacl-watcher".into())
        .spawn(move || {
            // Load NT native APIs dynamically.
            // NtQueryInformationProcess(ProcessHandleInformation=51): returns handles
            //   for ONE specific process only — much faster than system-wide enumeration.
            // NtQueryObject(ObjectNameInformation=1): returns the NT path of a handle.
            type NtQueryInfoProcess =
                unsafe extern "system" fn(isize, u32, *mut c_void, u32, *mut u32) -> i32;
            type NtQueryObj =
                unsafe extern "system" fn(isize, u32, *mut c_void, u32, *mut u32) -> i32;

            let (nt_qip, nt_qo): (NtQueryInfoProcess, NtQueryObj) = unsafe {
                let ntdll = match GetModuleHandleW(windows::core::w!("ntdll.dll")).ok() {
                    Some(h) => h,
                    None => {
                        log::error!("jack-pipe-dacl-watcher: GetModuleHandleW(ntdll) failed");
                        return;
                    }
                };
                let qip =
                    GetProcAddress(ntdll, windows::core::s!("NtQueryInformationProcess"));
                let qo = GetProcAddress(ntdll, windows::core::s!("NtQueryObject"));
                match (qip, qo) {
                    (Some(a), Some(b)) => (std::mem::transmute(a), std::mem::transmute(b)),
                    _ => {
                        log::error!("jack-pipe-dacl-watcher: NT API lookup failed");
                        return;
                    }
                }
            };

            // Build null-DACL security descriptor (reused for every patch).
            let mut sd = unsafe { std::mem::zeroed::<SECURITY_DESCRIPTOR>() };
            let psd = PSECURITY_DESCRIPTOR(&mut sd as *mut SECURITY_DESCRIPTOR as *mut _);
            if unsafe { InitializeSecurityDescriptor(psd, 1) }.is_err()
                || unsafe { SetSecurityDescriptorDacl(psd, BOOL(1), None, BOOL(0)) }.is_err()
            {
                log::error!("jack-pipe-dacl-watcher: failed to build null DACL SD");
                return;
            }

            // PROCESS_HANDLE_TABLE_ENTRY_INFO (64-bit layout, 40 bytes):
            //   void*  handle_value       (offset  0, 8 bytes)
            //   usize  handle_count       (offset  8, 8 bytes)
            //   usize  pointer_count      (offset 16, 8 bytes)
            //   u32    granted_access     (offset 24, 4 bytes)
            //   u32    object_type_index  (offset 28, 4 bytes)
            //   u32    handle_attributes  (offset 32, 4 bytes)
            //   u32    reserved           (offset 36, 4 bytes)
            #[repr(C)]
            #[derive(Copy, Clone)]
            struct ProcHandleEntry {
                handle_value: usize,
                _handle_count: usize,
                _pointer_count: usize,
                _granted_access: u32,
                _object_type_index: u32,
                _handle_attributes: u32,
                _reserved: u32,
            }

            // PROCESS_HANDLE_SNAPSHOT_INFORMATION header (64-bit, 16 bytes):
            //   usize  number_of_handles  (offset 0)
            //   usize  reserved           (offset 8)
            #[repr(C)]
            struct ProcHandleSnap {
                number_of_handles: usize,
                _reserved: usize,
            }

            // UNICODE_STRING (64-bit): Length(u16), MaxLen(u16), pad(u32), Buffer(*u16)
            #[repr(C)]
            struct UnicodeString {
                length: u16,
                _maximum_length: u16,
                _pad: u32,
                buffer: *const u16,
            }

            // NT pipe path prefix returned by NtQueryObject for named pipes.
            let prefix: Vec<u16> =
                "\\Device\\NamedPipe\\client_jack_orender_".encode_utf16().collect();

            let proc = unsafe { GetCurrentProcess() };
            let mut patched = [false; 16];

            loop {
                if sys::ShutdownHandle::is_requested() {
                    break;
                }
                if patched.iter().all(|&p| p) {
                    break;
                }

                // Query only our process's handles (ProcessHandleInformation = 51).
                let mut buf: Vec<u8> = vec![0u8; 64 * 1024];
                let mut ret_len: u32 = 0;
                loop {
                    let s = unsafe {
                        nt_qip(
                            proc.0,
                            51, // ProcessHandleInformation
                            buf.as_mut_ptr() as _,
                            buf.len() as u32,
                            &mut ret_len,
                        )
                    };
                    if s == 0 {
                        break;
                    }
                    // STATUS_INFO_LENGTH_MISMATCH = 0xC0000004
                    if s == 0xC0000004u32 as i32 {
                        let new_len = (ret_len as usize + 4096).max(buf.len() * 2);
                        buf.resize(new_len, 0);
                    } else {
                        break;
                    }
                }

                let snap = unsafe { &*(buf.as_ptr() as *const ProcHandleSnap) };
                let count = snap.number_of_handles;
                let entries: &[ProcHandleEntry] = unsafe {
                    let ptr = buf
                        .as_ptr()
                        .add(std::mem::size_of::<ProcHandleSnap>())
                        as *const ProcHandleEntry;
                    std::slice::from_raw_parts(ptr, count)
                };

                for entry in entries {
                    let handle = HANDLE(entry.handle_value as isize);

                    // Pre-filter: only call NtQueryObject on pipe handles.
                    // GetFileType on non-file handles returns FILE_TYPE_UNKNOWN (0)
                    // and is safe to call on any handle type without hanging.
                    // FILE_TYPE_PIPE = 3
                    if unsafe { GetFileType(handle) }.0 != 3 {
                        continue;
                    }

                    // Get the NT object name.  Safe to call on named pipe handles.
                    let mut name_buf = vec![0u16; 512];
                    let mut nret: u32 = 0;
                    let ns = unsafe {
                        nt_qo(
                            handle.0,
                            1, // ObjectNameInformation
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
                    let name = unsafe { std::slice::from_raw_parts(us.buffer, name_chars) };

                    if name.len() <= prefix.len() || name[..prefix.len()] != *prefix {
                        continue;
                    }

                    let suffix = String::from_utf16_lossy(&name[prefix.len()..]);
                    let idx: usize = match suffix.parse() {
                        Ok(n) if n < 16 => n,
                        _ => continue,
                    };
                    if patched[idx] {
                        continue;
                    }

                    // Patch directly on the server-side handle libjack holds.
                    // No client connection is made; the pipe instance stays available.
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

                // Poll at 200 µs to catch the pipe within the narrow window
                // between libjack's CreateNamedPipe and jackd's connect attempt.
                std::thread::sleep(std::time::Duration::from_micros(200));
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
