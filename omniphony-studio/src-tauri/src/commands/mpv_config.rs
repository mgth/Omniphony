//! Toggle `ad=orender` in the user's mpv config.
//!
//! mpv only selects the orender decoder when asked to (`--ad=orender`), so a
//! fresh install plays through mpv's own decoders and the engine is never used.
//! The one-line opt-in lives in `mpv.conf`, which is why Studio can offer it.
//!
//! That file belongs to another application and is usually hand-maintained, so
//! the rules here are deliberately conservative:
//!
//! * Everything Studio writes lives between two marker comments. Nothing outside
//!   the block is ever reformatted, reordered or removed.
//! * The block is inserted in the **global** section — before the first
//!   `[profile]` header. Appending at the end of the file would land inside
//!   whatever profile happens to be last, silently scoping `ad=orender` to it.
//! * A pre-existing `ad=` written by the user is never touched. It wins, and the
//!   toggle reports a conflict instead of fighting over the option.

use serde::Serialize;
use std::path::{Path, PathBuf};

const BEGIN: &str = "# >>> omniphony (managed) >>>";
const END: &str = "# <<< omniphony (managed) <<<";
const OPTION: &str = "ad=orender";

/// What the config currently says about the orender decoder.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum MpvOrenderState {
    /// Managed block present, option active.
    Enabled,
    /// Managed block present, option commented out.
    Disabled,
    /// No managed block; nothing sets `ad=`.
    Absent,
    /// An `ad=` outside the managed block. Studio refuses to touch it.
    Conflict,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MpvOrenderStatus {
    /// Absolute path to the config, whether or not it exists yet.
    pub path: String,
    pub exists: bool,
    pub state: MpvOrenderState,
    /// 1-based line of the conflicting `ad=`, when `state` is `conflict`.
    pub conflict_line: Option<usize>,
    /// Verbatim conflicting line, so the UI can show what it found.
    pub conflict_text: Option<String>,
}

/// `$MPV_HOME`, else `$XDG_CONFIG_HOME/mpv`, else `~/.config/mpv` — the lookup
/// mpv itself does on Unix. Windows uses `%APPDATA%\mpv`.
fn mpv_config_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("MPV_HOME") {
        return Some(PathBuf::from(home));
    }
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("mpv"))
    }
    #[cfg(not(windows))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            return Some(PathBuf::from(xdg).join("mpv"));
        }
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/mpv"))
    }
}

fn mpv_config_path() -> Result<PathBuf, String> {
    Ok(mpv_config_dir()
        .ok_or("cannot determine the mpv config directory")?
        .join("mpv.conf"))
}

/// Is this line an active (uncommented) `ad=` setting?
fn is_active_ad_line(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with('#') {
        return false;
    }
    let Some(rest) = t.strip_prefix("ad") else {
        return false;
    };
    rest.trim_start().starts_with('=')
}

/// Half-open range of the managed block, if present.
fn managed_block(lines: &[String]) -> Option<(usize, usize)> {
    let begin = lines.iter().position(|l| l.trim() == BEGIN)?;
    let end = lines
        .iter()
        .skip(begin + 1)
        .position(|l| l.trim() == END)
        .map(|i| begin + 1 + i)?;
    Some((begin, end))
}

fn classify(lines: &[String]) -> (MpvOrenderState, Option<usize>, Option<String>) {
    let block = managed_block(lines);
    // Any active `ad=` outside our block is the user's; it takes precedence.
    for (i, line) in lines.iter().enumerate() {
        if let Some((b, e)) = block {
            if i >= b && i <= e {
                continue;
            }
        }
        if is_active_ad_line(line) {
            return (MpvOrenderState::Conflict, Some(i + 1), Some(line.clone()));
        }
    }
    let Some((b, e)) = block else {
        return (MpvOrenderState::Absent, None, None);
    };
    let enabled = lines[b + 1..e].iter().any(|l| is_active_ad_line(l));
    let state = if enabled {
        MpvOrenderState::Enabled
    } else {
        MpvOrenderState::Disabled
    };
    (state, None, None)
}

fn read_lines(path: &Path) -> Result<Option<Vec<String>>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text.lines().map(|l| l.to_string()).collect())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn status_from(path: &Path, lines: Option<&Vec<String>>) -> MpvOrenderStatus {
    let (state, conflict_line, conflict_text) = match lines {
        Some(l) => classify(l),
        None => (MpvOrenderState::Absent, None, None),
    };
    MpvOrenderStatus {
        path: path.display().to_string(),
        exists: lines.is_some(),
        state,
        conflict_line,
        conflict_text,
    }
}

/// Index to insert the managed block at: just before the first profile header,
/// so the option lands in the global section. Trailing blank lines before that
/// header are kept after the block rather than swallowed.
fn global_section_end(lines: &[String]) -> usize {
    let first_profile = lines
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .unwrap_or(lines.len());
    let mut at = first_profile;
    while at > 0 && lines[at - 1].trim().is_empty() {
        at -= 1;
    }
    at
}

/// Add, enable or comment out the managed option in `lines`, in place.
///
/// Pure so the file surgery is testable on its own: this is the part that must
/// never disturb a hand-maintained config.
fn apply(lines: &mut Vec<String>, enabled: bool) {
    let option_line = if enabled {
        OPTION.to_string()
    } else {
        format!("#{OPTION}")
    };
    match managed_block(lines) {
        Some((b, e)) => {
            // Rewrite only the block's interior; the markers stay put.
            lines.splice(b + 1..e, std::iter::once(option_line));
        }
        None => {
            let at = global_section_end(lines);
            let mut block = Vec::new();
            // Keep one blank line of separation when inserting after content.
            if at > 0 && !lines[at - 1].trim().is_empty() {
                block.push(String::new());
            }
            block.push(BEGIN.to_string());
            block.push(option_line);
            block.push(END.to_string());
            lines.splice(at..at, block);
        }
    }
}

/// Current state of `ad=orender` in the user's mpv config.
#[tauri::command]
pub fn mpv_orender_status() -> Result<MpvOrenderStatus, String> {
    let path = mpv_config_path()?;
    let lines = read_lines(&path)?;
    Ok(status_from(&path, lines.as_ref()))
}

/// Add, enable or comment out `ad=orender`, and report the resulting state.
///
/// Creates `mpv.conf` when missing. Refuses when the user already sets `ad=`
/// themselves: that line is theirs, and silently overriding it would change
/// playback behind their back.
#[tauri::command]
pub fn mpv_orender_set(enabled: bool) -> Result<MpvOrenderStatus, String> {
    let path = mpv_config_path()?;
    let existing = read_lines(&path)?;
    let mut lines = existing.clone().unwrap_or_default();

    let (state, line_no, text) = classify(&lines);
    if state == MpvOrenderState::Conflict {
        return Err(format!(
            "{} already sets the audio decoder at line {}: `{}`. \
             Studio will not overwrite a line you wrote — remove or comment it \
             out, then toggle this again.",
            path.display(),
            line_no.unwrap_or(0),
            text.unwrap_or_default().trim(),
        ));
    }

    apply(&mut lines, enabled);

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let mut out = lines.join("\n");
    out.push('\n');
    std::fs::write(&path, out).map_err(|e| format!("cannot write {}: {e}", path.display()))?;

    let lines = read_lines(&path)?;
    Ok(status_from(&path, lines.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Vec<String> {
        s.lines().map(|l| l.to_string()).collect()
    }

    #[test]
    fn detects_an_absent_option() {
        assert_eq!(classify(&v("vo=gpu\nvolume=70")).0, MpvOrenderState::Absent);
    }

    #[test]
    fn detects_enabled_and_disabled_inside_the_block() {
        let on = format!("vo=gpu\n{BEGIN}\n{OPTION}\n{END}");
        assert_eq!(classify(&v(&on)).0, MpvOrenderState::Enabled);
        let off = format!("vo=gpu\n{BEGIN}\n#{OPTION}\n{END}");
        assert_eq!(classify(&v(&off)).0, MpvOrenderState::Disabled);
    }

    /// A hand-written `ad=` is the user's call; the toggle must stand down.
    #[test]
    fn a_foreign_ad_line_is_a_conflict() {
        let (state, line, text) = classify(&v("vo=gpu\nad=lavc\nvolume=70"));
        assert_eq!(state, MpvOrenderState::Conflict);
        assert_eq!(line, Some(2));
        assert_eq!(text.as_deref(), Some("ad=lavc"));
    }

    #[test]
    fn commented_and_lookalike_lines_are_not_conflicts() {
        for s in [
            "#ad=lavc",
            "  # ad=lavc",
            "adapter=1",
            "vad=x",
            "audio-display=no",
        ] {
            let src = format!("vo=gpu\n{s}");
            assert_eq!(
                classify(&v(&src)).0,
                MpvOrenderState::Absent,
                "misread as a conflict: {s}"
            );
        }
    }

    /// The whole point: a config ending inside a profile must not receive the
    /// option at EOF, or it would only apply while that profile is active.
    #[test]
    fn the_block_goes_in_the_global_section_not_the_last_profile() {
        let conf = v("vo=gpu\nvolume=70\n\n[hdr]\ntarget-peak=auto\ncontrast=0");
        let at = global_section_end(&conf);
        assert_eq!(at, 2, "must insert before the blank line preceding [hdr]");
        assert!(conf[at..].iter().any(|l| l.starts_with('[')));
    }

    #[test]
    fn a_config_with_no_profile_appends_at_the_end() {
        let conf = v("vo=gpu\nvolume=70");
        assert_eq!(global_section_end(&conf), 2);
    }

    #[test]
    fn an_empty_config_inserts_at_the_top() {
        assert_eq!(global_section_end(&[]), 0);
    }

    // A config shaped like a real one: global options, then profiles, ending
    // inside the last profile. This is the layout that makes a naive append
    // wrong.
    fn realistic() -> Vec<String> {
        v("vo=gpu\nhwdec=auto\nvolume=70\n\n[upscale-ravu-hq]\nglsl-shaders=ravu.hook\n\n[hdr-bright]\ntarget-peak=auto\ncontrast=0")
    }

    #[test]
    fn enabling_puts_the_option_before_the_first_profile() {
        let mut lines = realistic();
        apply(&mut lines, true);
        let opt = lines
            .iter()
            .position(|l| l == OPTION)
            .expect("option written");
        let first_profile = lines
            .iter()
            .position(|l| l.trim_start().starts_with('['))
            .expect("profiles kept");
        assert!(
            opt < first_profile,
            "option landed inside a profile: it would only apply while that profile is active"
        );
        assert_eq!(classify(&lines).0, MpvOrenderState::Enabled);
    }

    /// The user's own content must survive verbatim and in order. Blank-line
    /// spacing around the inserted block is cosmetic and deliberately not
    /// asserted; altering, reordering or dropping a real line is the failure
    /// this guards against.
    #[test]
    fn no_user_line_is_altered_reordered_or_lost() {
        let before = realistic();
        let mut after = before.clone();
        apply(&mut after, true);
        let (b, e) = managed_block(&after).unwrap();

        let significant = |src: &[String]| -> Vec<String> {
            src.iter()
                .filter(|l| !l.trim().is_empty())
                .cloned()
                .collect()
        };
        let mut outside: Vec<String> = after[..b].to_vec();
        outside.extend_from_slice(&after[e + 1..]);

        assert_eq!(
            significant(&outside),
            significant(&before),
            "the surrounding config was modified"
        );
    }

    #[test]
    fn toggling_off_then_on_is_stable() {
        let mut lines = realistic();
        apply(&mut lines, true);
        let enabled_once = lines.clone();
        apply(&mut lines, false);
        assert_eq!(classify(&lines).0, MpvOrenderState::Disabled);
        apply(&mut lines, true);
        assert_eq!(lines, enabled_once, "round-trip must not drift the file");
        // And it must not grow a second block each time.
        assert_eq!(lines.iter().filter(|l| l.trim() == BEGIN).count(), 1);
    }

    #[test]
    fn an_empty_config_gets_a_block_with_no_leading_blank() {
        let mut lines: Vec<String> = Vec::new();
        apply(&mut lines, true);
        assert_eq!(
            lines,
            vec![BEGIN.to_string(), OPTION.to_string(), END.to_string()]
        );
    }
}
