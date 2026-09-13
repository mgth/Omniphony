//! Browse the sofacoustics.org SOFA database and manage the local HRTF cache.
//!
//! Ported from `src-tauri/src/commands/sofa_browser.rs`. The site serves plain
//! Apache directory indexes under a fixed root; we list entries
//! (subdirectories + `.sofa` files), download a chosen file into the cache
//! directory, and the caller then activates it through the existing
//! `control_hrir_source` command (`sofa:<local path>`).
//!
//! Two things the Tauri host got from its framework are parameters here. The
//! cache directory is passed in rather than resolved from an `AppHandle`, and
//! download progress is a callback rather than a webview event — the native
//! browser is one window, so the progress goes straight to the widget that
//! draws it.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rosc::OscType;

use super::{OscControlMsg, SharedState, send_control};

/// Fixed browse root. `path` arguments are relative to this and sanitised —
/// the browser can never escape it.
pub const ROOT: &str = "https://sofacoustics.org/data/";

/// One row of an index page.
#[derive(Clone, Debug)]
pub struct SofaEntry {
    /// Percent-encoded path segment as found in the index (append to the
    /// current path for navigation/download).
    pub href: String,
    /// Human-readable (percent-decoded) name.
    pub name: String,
    pub dir: bool,
    /// Size column as shown by the index ("1.6M", "-" for dirs).
    pub size: String,
}

pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Reject anything that could walk out of the root.
pub fn sanitize(rel: &str) -> Result<String, String> {
    let decoded = percent_decode(rel);
    if rel.starts_with('/') || rel.contains("://") {
        return Err("invalid path".into());
    }
    for seg in decoded.split('/') {
        if seg == ".." {
            return Err("invalid path".into());
        }
    }
    Ok(rel.to_string())
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout_read(std::time::Duration::from_secs(60))
        .build()
}

pub fn parse_index(html: &str) -> Vec<SofaEntry> {
    let mut entries = Vec::new();
    for line in html.lines() {
        let Some(h0) = line.find("href=\"") else {
            continue;
        };
        let rest = &line[h0 + 6..];
        let Some(h1) = rest.find('"') else { continue };
        let href = &rest[..h1];
        // Skip sort links, parent/absolute links, and external URLs.
        if href.is_empty()
            || href.starts_with('?')
            || href.starts_with('/')
            || href.starts_with('#')
            || href.contains("://")
            || href == "../"
        {
            continue;
        }
        let dir = href.ends_with('/');
        let name = percent_decode(href.trim_end_matches('/'));
        // Files: only .sofa is loadable; hide the rest (docs, meshes, csv…).
        if !dir && !name.to_ascii_lowercase().ends_with(".sofa") {
            continue;
        }
        // Size = second right-aligned cell of the row (first is the date).
        let mut sizes = line
            .split("<td align=\"right\">")
            .skip(2)
            .map(|c| c.split('<').next().unwrap_or("").trim().to_string());
        let size = sizes.next().unwrap_or_default();
        entries.push(SofaEntry {
            href: href.to_string(),
            name,
            dir,
            size,
        });
    }
    // Directories first, then files, each alphabetically.
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then(a.name.cmp(&b.name)));
    entries
}

/// List one directory of the SOFA database. `path` is the percent-encoded
/// path relative to the root ("" = root, "database/hutubs/" …). Blocking:
/// the caller runs it on a worker thread.
pub fn browse(path: &str) -> Result<Vec<SofaEntry>, String> {
    let rel = sanitize(path)?;
    let url = format!("{ROOT}{rel}");
    let body = agent()
        .get(&url)
        .call()
        .map_err(|e| format!("fetch {url}: {e}"))?
        .into_string()
        .map_err(|e| format!("read {url}: {e}"))?;
    Ok(parse_index(&body))
}

/// The global attributes that matter for licensing and attribution.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SofaMeta {
    pub license: String,
    pub author: String,
    pub organization: String,
}

#[derive(Clone, Debug)]
pub struct LocalSofa {
    pub name: String,
    pub path: PathBuf,
    pub size: String,
    pub meta: SofaMeta,
}

/// Read the SOFA global attributes that matter for licensing/attribution.
/// Parsing a big file is not free, so the result is cached in a JSON sidecar
/// (`<file>.meta.json`) written next to the .sofa.
pub fn file_meta(path: &Path) -> SofaMeta {
    let sidecar = path.with_extension("sofa.meta.json");
    if let Ok(bytes) = std::fs::read(&sidecar)
        && let Ok(meta) = serde_json::from_slice::<SofaMeta>(&bytes)
    {
        return meta;
    }
    let meta = match sofar::reader::Sofar::open(path) {
        Ok(sofa) => {
            let attrs = &sofa.hrtf().attributes;
            let get = |k: &str| attrs.get(k).cloned().unwrap_or_default();
            SofaMeta {
                license: get("License"),
                author: get("AuthorContact"),
                organization: get("Organization"),
            }
        }
        Err(_) => SofaMeta::default(),
    };
    if let Ok(bytes) = serde_json::to_vec(&meta) {
        let _ = std::fs::write(&sidecar, bytes);
    }
    meta
}

/// List the already-downloaded `.sofa` files in the cache directory. This is
/// the default browser view — no network involved.
pub fn list_local(dir: &Path) -> Result<Vec<LocalSofa>, String> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(out), // dir not created yet → empty list
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.to_ascii_lowercase().ends_with(".sofa") {
            continue;
        }
        let size = entry
            .metadata()
            .map(|m| human_size(m.len()))
            .unwrap_or_default();
        let meta = file_meta(&path);
        out.push(LocalSofa {
            name,
            path,
            size,
            meta,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// The listing's size column: megabytes once there is a megabyte to show.
pub fn human_size(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 1.0 {
        format!("{mb:.1} MB")
    } else {
        format!("{:.0} kB", bytes as f64 / 1024.0)
    }
}

/// Delete cached .sofa files (and their metadata sidecars). Paths must live
/// inside the cache dir — anything else is rejected.
pub fn delete_local(dir: &Path, paths: &[PathBuf]) -> Result<(), String> {
    for path in paths {
        if path.parent() != Some(dir) {
            return Err(format!(
                "refusing to delete outside the cache: {}",
                path.display()
            ));
        }
        if !path
            .to_string_lossy()
            .to_ascii_lowercase()
            .ends_with(".sofa")
        {
            return Err(format!("not a .sofa file: {}", path.display()));
        }
        std::fs::remove_file(path).map_err(|e| format!("delete {}: {e}", path.display()))?;
        let _ = std::fs::remove_file(path.with_extension("sofa.meta.json"));
    }
    Ok(())
}

/// Copy a user-picked .sofa file from anywhere on this machine into the
/// cache dir (metadata sidecar is built on the next listing).
pub fn import_local(dir: &Path, src: &Path) -> Result<PathBuf, String> {
    let name = src
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !name.to_ascii_lowercase().ends_with(".sofa") {
        return Err(format!("not a .sofa file: {}", src.display()));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let dest = dir.join(&name);
    std::fs::copy(src, &dest).map_err(|e| format!("copy {}: {e}", src.display()))?;
    let _ = std::fs::remove_file(dest.with_extension("sofa.meta.json"));
    Ok(dest)
}

/// Push a cached .sofa to the renderer over OSC (chunked blobs) — for setups
/// where Studio does not share a filesystem with the renderer. The renderer
/// stores it under its own config dir and activates it on completion.
///
/// Takes the control sender rather than the whole `SharedState` so the upload
/// can run on a worker thread: a file is up to a gigabyte, and chunking it on
/// the frame loop would stop the window for the duration.
/// Send a cached SOFA file to the renderer in chunks.
///
/// Takes the state rather than the channel: handing a `ControlTx` out to a
/// caller is handing out the ability to send anything, which is the one thing
/// the UI is not allowed to have.
pub fn upload_to_renderer(state: &SharedState, dir: &Path, path: &Path) -> Result<u32, String> {
    let tx = &state.osc_tx;
    if path.parent() != Some(dir) {
        return Err("upload source must be a cached file".into());
    }
    let name = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let data = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let total = data.len();
    if total == 0 || total > (1 << 30) {
        return Err(format!("bad file size: {total}"));
    }
    const CHUNK: usize = 32 * 1024;
    send_control(
        tx,
        OscControlMsg::SendArgs {
            address: "/omniphony/control/binaural/hrtf_upload/begin".to_string(),
            args: vec![OscType::String(name), OscType::Int(total as i32)],
        },
    );
    let mut seq: i32 = 0;
    for chunk in data.chunks(CHUNK) {
        send_control(
            tx,
            OscControlMsg::SendArgs {
                address: "/omniphony/control/binaural/hrtf_upload/chunk".to_string(),
                args: vec![OscType::Int(seq), OscType::Blob(chunk.to_vec())],
            },
        );
        seq += 1;
        // UDP politeness: don't burst hundreds of datagrams back-to-back.
        if seq % 16 == 0 {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    send_control(
        tx,
        OscControlMsg::SendArgs {
            address: "/omniphony/control/binaural/hrtf_upload/end".to_string(),
            args: vec![OscType::Int(seq)],
        },
    );
    Ok(seq as u32)
}

/// Download one `.sofa` file into the cache dir and return its local path. The
/// file name flattens the relative path so different databases cannot collide.
/// Streams in chunks, reporting `(bytes, total)` through `progress` and
/// honouring `cancel`. No size limit — some database entries are hundreds of
/// MB; the progress bar and the cancel button are the safety valve. An
/// already-downloaded file whose size matches the server's Content-Length is
/// reused as-is.
pub fn download(
    dir: &Path,
    rel: &str,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<PathBuf, String> {
    let rel = sanitize(rel)?;
    if !percent_decode(&rel).to_ascii_lowercase().ends_with(".sofa") {
        return Err("not a .sofa file".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let flat = percent_decode(&rel).replace('/', "_");
    let dest = dir.join(flat);
    let url = format!("{ROOT}{rel}");
    let resp = agent()
        .get(&url)
        .call()
        .map_err(|e| format!("fetch {url}: {e}"))?;
    let total: Option<u64> = resp.header("Content-Length").and_then(|v| v.parse().ok());

    // Cached copy with the expected size → reuse, no network transfer.
    if let (Some(expected), Ok(meta)) = (total, std::fs::metadata(&dest))
        && meta.len() == expected
    {
        progress(expected, Some(expected));
        return Ok(dest);
    }

    let mut reader = resp.into_reader();
    let tmp = dest.with_extension("part");
    let mut file =
        std::fs::File::create(&tmp).map_err(|e| format!("create {}: {e}", tmp.display()))?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut done: u64 = 0;
    let mut last_emit = std::time::Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(file);
            let _ = std::fs::remove_file(&tmp);
            return Err("cancelled".into());
        }
        let n = reader.read(&mut buf).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("download {url}: {e}")
        })?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("write {}: {e}", tmp.display())
        })?;
        done += n as u64;
        if last_emit.elapsed() >= std::time::Duration::from_millis(100) {
            last_emit = std::time::Instant::now();
            progress(done, total);
        }
    }
    progress(done, total);
    std::fs::rename(&tmp, &dest).map_err(|e| format!("finalize {}: {e}", dest.display()))?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_apache_index_rows() {
        let html = r#"
<tr><td><img alt="[PARENTDIR]"></td><td><a href="/data/">Parent Directory</a></td><td>&nbsp;</td><td align="right">  - </td></tr>
<tr><td><img alt="[DIR]"></td><td><a href="hutubs/">hutubs/</a></td><td align="right">2020-01-01 10:00  </td><td align="right">  - </td></tr>
<tr><td><img alt="[FILE]"></td><td><a href="pp1_HRIRs_measured.sofa">pp1_HRIRs_measured.sofa</a></td><td align="right">2020-01-01 10:00  </td><td align="right">1.6M</td></tr>
<tr><td><img alt="[FILE]"></td><td><a href="Documentation.pdf">Documentation.pdf</a></td><td align="right">2020-01-01 10:00  </td><td align="right">2M</td></tr>
<tr><th><a href="?C=N;O=D">Name</a></th></tr>
"#;
        let entries = parse_index(html);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].dir && entries[0].name == "hutubs");
        assert!(!entries[1].dir && entries[1].name == "pp1_HRIRs_measured.sofa");
        assert_eq!(entries[1].size, "1.6M");
    }

    #[test]
    fn sanitize_rejects_escapes() {
        assert!(sanitize("../etc/").is_err());
        assert!(sanitize("a/%2e%2e/b").is_err());
        assert!(sanitize("/abs").is_err());
        assert!(sanitize("http://x").is_err());
        assert!(sanitize("database/hutubs/").is_ok());
    }

    #[test]
    fn decodes_percent_names() {
        assert_eq!(
            percent_decode("aachen%20(high-resolution)"),
            "aachen (high-resolution)"
        );
    }

    /// Deleting is confined to the cache directory, and to `.sofa` files: the
    /// list the button acts on comes from the cache, but the path is a string
    /// by the time it gets here.
    #[test]
    fn delete_refuses_anything_outside_the_cache() {
        let dir = Path::new("/cache/hrtf");
        assert!(delete_local(dir, &[PathBuf::from("/etc/passwd")]).is_err());
        assert!(delete_local(dir, &[PathBuf::from("/cache/hrtf/notes.txt")]).is_err());
        assert!(delete_local(dir, &[PathBuf::from("/cache/other/a.sofa")]).is_err());
    }

    #[test]
    fn sizes_switch_unit_at_a_megabyte() {
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
        assert_eq!(human_size(512 * 1024), "512 kB");
    }
}
