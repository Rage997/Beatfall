//! Live audio capture.
//!
//! Linux: PulseAudio / PipeWire-Pulse via `pactl` (source discovery, JSON) and
//! `parec` (raw float32le mono stream). Unix: raw f32le mono PCM over stdin.
//!
//! Every capture runs on a worker thread that polls its input with short
//! timeouts, so `Audio::stop` interrupts promptly and `Audio::drop` (which
//! joins the worker) never waits for new audio. Workers own their child process
//! or duplicated stdin descriptor and clean up before returning; reads are
//! bounded and the only consumer is the fixed-size `Shared` ring.

use crate::audio::{Audio, Shared};
#[cfg(unix)]
use anyhow::Context;
use anyhow::{bail, Result};
use std::time::Duration;

#[cfg(unix)]
use std::{
    fs::File,
    io::{self, IsTerminal, Read},
    os::fd::BorrowedFd,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
};

#[cfg(target_os = "linux")]
use std::{
    process::{Command, Stdio},
    thread::JoinHandle,
};

#[cfg(unix)]
/// Max bytes pulled from a capture source per poll iteration.
const CHUNK: usize = 16384;
#[cfg(unix)]
/// Poll interval so the worker notices `stop` within ~100 ms.
const POLL_TIMEOUT: Duration = Duration::from_millis(100);

/// One PulseAudio/PipeWire source as reported by `pactl --format=json list sources`.
#[cfg(target_os = "linux")]
#[derive(Clone)]
struct Source {
    index: u64,
    name: String,
    description: String,
    /// Sink this source mirrors; `Some` means a playback monitor (system output).
    monitor_of: Option<String>,
}

#[cfg(target_os = "linux")]
impl Source {
    fn is_monitor(&self) -> bool {
        self.monitor_of.is_some()
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Open a live capture. On Linux the default source is the playback monitor of
/// the default sink (never silently a microphone); `source` selects by name or
/// numeric id; `microphone` restricts selection to non-monitor sources.
pub fn open(source: Option<&str>, microphone: bool, rate: u32) -> Result<Audio> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (source, microphone, rate);
        bail!(
            "live capture is Linux-only (PulseAudio or PipeWire-Pulse).\n\
             Use --demo for synthetic audio, or pipe raw f32le mono PCM to --stdin on Unix.\n\
             Direct macOS/Windows audio capture is not implemented."
        );
    }
    #[cfg(target_os = "linux")]
    {
        let sources = list_source_objects()?;
        // Discovery facts are best-effort: an explicit --source or --microphone
        // must keep working even when the server has no default sink/source.
        let (default_sink, default_source) = pactl_info();
        let selected = select_source(
            source,
            microphone,
            default_sink.as_deref(),
            default_source.as_deref(),
            &sources,
        )?;
        verify_parec()?;
        let kind = if selected.is_monitor() {
            "PLAYBACK"
        } else {
            "INPUT"
        };
        let mut audio = Audio::empty(format!("LIVE | {kind} | {} | {rate} Hz", selected.name));
        let shared = audio.shared.clone();
        let stop = audio.stop.clone();
        audio.worker = Some(spawn_parec_worker(selected.name, rate, shared, stop));
        Ok(audio)
    }
}

/// Read raw little-endian float32 mono PCM from stdin at `rate` Hz.
pub fn stdin(rate: u32) -> Result<Audio> {
    #[cfg(not(unix))]
    {
        let _ = rate;
        bail!("--stdin requires a Unix pipe of raw f32le mono PCM; use --demo on this platform");
    }
    #[cfg(unix)]
    {
        if io::stdin().is_terminal() {
            bail!(
                "stdin is a terminal — pipe raw float32le mono PCM at {rate} Hz, e.g. \
                 `parec --raw --format=float32le --rate={rate} --channels=1 | cava-with-ball --stdin --sample-rate {rate}`"
            );
        }
        // Do not hold Rust's global stdin lock: terminal libraries may need it.
        // SAFETY: fd 0 is borrowed only long enough to duplicate it; ownership
        // stays with the process and the worker owns only the duplicate.
        let fd = unsafe { BorrowedFd::borrow_raw(libc::STDIN_FILENO) }
            .try_clone_to_owned()
            .context("could not duplicate PCM stdin")?;
        let input = File::from(fd);
        let mut audio = Audio::empty(format!("STDIN | raw f32le mono | {rate} Hz"));
        let shared = audio.shared.clone();
        let stop = audio.stop.clone();
        audio.worker = Some(thread::spawn(move || stdin_worker(input, shared, stop)));
        Ok(audio)
    }
}

/// Print all capture sources, classified as playback monitors vs inputs.
pub fn list_sources() -> Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        bail!("source discovery is Linux-only (PulseAudio or PipeWire-Pulse); use --demo or Unix PCM --stdin here");
    }
    #[cfg(target_os = "linux")]
    {
        use anyhow::Context;
        use std::io::Write;
        let sources = list_source_objects()?;
        let (default_sink, _) = pactl_info();
        let is_default = |s: &Source| {
            default_sink.as_deref().is_some_and(|sink| {
                s.monitor_of.as_deref() == Some(sink) || s.name == format!("{sink}.monitor")
            })
        };
        let monitors: Vec<&Source> = sources.iter().filter(|s| s.is_monitor()).collect();
        let inputs: Vec<&Source> = sources.iter().filter(|s| !s.is_monitor()).collect();

        let mut text = String::new();
        text.push_str("playback monitors — capture what the system plays:\n");
        if monitors.is_empty() {
            text.push_str("  (none)\n");
        }
        for s in &monitors {
            let marker = if is_default(s) { "   * default" } else { "" };
            text.push_str(&source_listing_line(s.index, &s.name, &s.description));
            text.push_str(marker);
            text.push('\n');
        }
        text.push('\n');
        text.push_str("inputs — microphones and other capture devices:\n");
        if inputs.is_empty() {
            text.push_str("  (none)\n");
        }
        for s in &inputs {
            text.push_str(&source_listing_line(s.index, &s.name, &s.description));
            text.push('\n');
        }
        text.push('\n');
        text.push_str("select with --source <name or id>; --microphone restricts to inputs\n");
        io::stdout()
            .write_all(text.as_bytes())
            .context("could not write to stdout")?;
        Ok(())
    }
}

#[cfg(any(target_os = "linux", test))]
fn source_listing_line(index: u64, name: &str, description: &str) -> String {
    format!(
        "  [{index}] {}  —  {}",
        name.escape_debug(),
        description.escape_debug()
    )
}

// ---------------------------------------------------------------------------
// pactl discovery (Linux)
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn pactl_json(args: &[&str]) -> Result<serde_json::Value> {
    use anyhow::Context;
    let out = Command::new("pactl")
        .arg("--format=json")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context(
            "pactl not found — install the PulseAudio client tools (pulseaudio-utils on \
             Debian/Ubuntu, including PipeWire-Pulse desktops); otherwise use --demo or --stdin",
        )?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        bail!(
            "pactl {} failed: {}",
            args.join(" "),
            if err.is_empty() {
                "no audio server running?"
            } else {
                err.as_str()
            }
        );
    }
    serde_json::from_str(stdout.trim())
        .with_context(|| format!("pactl {} returned invalid JSON", args.join(" ")))
}

/// Best-effort default sink/source names: `pactl info --format=json`
/// (`default_sink_name`/`default_source_name`) first, falling back to the
/// plain `pactl get-default-sink` / `get-default-source` text output, which
/// some pactl versions emit as plain text (or not at all) under `--format=json`.
#[cfg(target_os = "linux")]
fn pactl_info() -> (Option<String>, Option<String>) {
    let (mut sink, mut source) = match pactl_json(&["info"]) {
        Ok(value) => (
            json_name(&value, "default_sink_name"),
            json_name(&value, "default_source_name"),
        ),
        Err(_) => (None, None),
    };
    if sink.is_none() {
        sink = plain_pactl("get-default-sink");
    }
    if source.is_none() {
        source = plain_pactl("get-default-source");
    }
    (sink, source)
}

#[cfg(target_os = "linux")]
fn json_name(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| match v {
        serde_json::Value::String(s) if !s.is_empty() && s != "n/a" => Some(s.clone()),
        _ => None,
    })
}

#[cfg(target_os = "linux")]
fn plain_pactl(command: &str) -> Option<String> {
    let out = Command::new("pactl")
        .arg(command)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() || name == "n/a" {
        None
    } else {
        Some(name)
    }
}

/// pactl reports the mirrored sink of a monitor source via `monitor_source`
/// (newer versions) or `monitor_of_sink` (older); the value may be a sink
/// name, a numeric reference, `null`, or an empty string for non-monitors.
/// Only a usable value marks a source as a playback monitor.
#[cfg(target_os = "linux")]
fn monitor_of_field(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s)
            if !s.is_empty() && s != "n/a" && s != "-1" && s != "4294967295" =>
        {
            Some(s.clone())
        }
        serde_json::Value::Number(n) => n
            .as_u64()
            .filter(|&id| id < u32::MAX as u64)
            .map(|id| id.to_string()),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn list_source_objects() -> Result<Vec<Source>> {
    let value = pactl_json(&["list", "sources"])?;
    let array = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("pactl list sources: expected a JSON array"))?;
    Ok(array
        .iter()
        .filter_map(|s| {
            let name = s.get("name")?.as_str()?.to_string();
            Some(Source {
                index: s.get("index").and_then(|i| i.as_u64()).unwrap_or(u64::MAX),
                description: s
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string(),
                monitor_of: s
                    .get("monitor_source")
                    .or_else(|| s.get("monitor_of_sink"))
                    .and_then(monitor_of_field),
                name,
            })
        })
        .collect())
}

#[cfg(target_os = "linux")]
fn select_source(
    source: Option<&str>,
    microphone: bool,
    default_sink: Option<&str>,
    default_source: Option<&str>,
    sources: &[Source],
) -> Result<Source> {
    match source {
        Some(query) => {
            let found = sources
                .iter()
                .find(|s| s.name == query || query == s.index.to_string())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no source named '{query}' or with id {query} — run --list-sources"
                    )
                })?;
            if microphone && found.is_monitor() {
                bail!(
                    "source '{}' is a playback monitor (system output), not an input; \
                     --microphone requires a non-monitor source",
                    found.name
                );
            }
            Ok(found.clone())
        }
        None if microphone => {
            let name = default_source.ok_or_else(|| {
                anyhow::anyhow!(
                    "no default input source — use --source <name or id> (see --list-sources)"
                )
            })?;
            let found = sources.iter().find(|s| s.name == name).ok_or_else(|| {
                anyhow::anyhow!("default input '{name}' not in source list — run --list-sources")
            })?;
            if found.is_monitor() {
                bail!(
                    "default input '{name}' is a playback monitor, not an input; \
                     use --source <name or id> (see --list-sources)"
                );
            }
            Ok(found.clone())
        }
        None => {
            let sink = default_sink.ok_or_else(|| {
                anyhow::anyhow!(
                    "no default audio sink — use --list-sources to pick a source, \
                     or --microphone for input capture"
                )
            })?;
            sources
                .iter()
                .find(|s| {
                    s.monitor_of.as_deref() == Some(sink) || s.name == format!("{sink}.monitor")
                })
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no playback monitor found for default sink '{sink}' — \
                         use --list-sources to pick a source, or --microphone for input capture"
                    )
                })
        }
    }
}

#[cfg(target_os = "linux")]
fn verify_parec() -> Result<()> {
    use anyhow::Context;
    Command::new("parec")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|_| ())
        .context(
            "parec not found — install the PulseAudio client tools (pulseaudio-utils on \
             Debian/Ubuntu, including PipeWire-Pulse desktops); otherwise use --demo or --stdin",
        )
}

// ---------------------------------------------------------------------------
// parec worker (Linux)
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn spawn_parec_worker(
    device: String,
    rate: u32,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut child = match Command::new("parec")
            .args([
                "--raw",
                "--format=float32le",
                "--channels=1",
                "--latency-msec=30",
            ])
            .arg(format!("--rate={rate}"))
            .arg(format!("--device={device}"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                set_status(
                    &shared,
                    &format!("CAPTURE FAILED • could not start parec: {e}"),
                );
                return;
            }
        };

        // Drain stderr on a side thread so a chatty server can never deadlock us.
        let stderr = child.stderr.take().expect("stderr was piped");
        let err_tail = Arc::new(Mutex::new(Vec::<u8>::new()));
        let tail_for_thread = err_tail.clone();
        let stderr_thread = thread::spawn(move || {
            let mut reader = io::BufReader::new(stderr);
            let mut buf = [0u8; 1024];
            let mut tail: Vec<u8> = Vec::with_capacity(512);
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        tail.extend_from_slice(&buf[..n]);
                        if tail.len() > 512 {
                            tail.drain(..tail.len() - 512);
                        }
                    }
                }
            }
            *tail_for_thread.lock().unwrap() = tail;
        });

        let mut stdout = child.stdout.take().expect("stdout was piped");
        let mut carry = [0u8; 4];
        let mut carry_len = 0usize;
        let mut chunk = [0u8; CHUNK];
        let mut read_failed = false;

        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            match poll_fd(&mut stdout, POLL_TIMEOUT) {
                PollState::Timeout => continue,
                PollState::Error(e) => {
                    read_failed = true;
                    set_status(&shared, &format!("CAPTURE ERROR • {e}"));
                    break;
                }
                PollState::Readable => {
                    match stdout.read(&mut chunk) {
                        Ok(0) => {
                            let msg = if carry_len > 0 {
                                format!("INPUT ENDED • {carry_len} trailing bytes dropped; bars decaying")
                            } else {
                                "INPUT ENDED • source closed; bars decaying".to_string()
                            };
                            set_status(&shared, &msg);
                            break;
                        }
                        Ok(n) => push_samples(&shared, &mut carry, &mut carry_len, &chunk[..n]),
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) => {
                            read_failed = true;
                            set_status(&shared, &format!("CAPTURE ERROR • {e}"));
                            break;
                        }
                    }
                }
            }
        }

        // Reap the child. If it exited on its own with an error (device lost,
        // server gone), surface that instead of a clean EOF; only kill when it
        // is still running (i.e. we are shutting down).
        let already_exited = child.try_wait().ok().flatten().is_some();
        if !already_exited {
            let _ = child.kill();
        }
        let exit = child.wait().ok();
        let _ = stderr_thread.join();
        let bad_exit = !stop.load(Ordering::Relaxed) && exit.as_ref().is_some_and(|s| !s.success());
        if read_failed || bad_exit {
            let tail = String::from_utf8_lossy(&err_tail.lock().unwrap())
                .trim()
                .to_string();
            if !tail.is_empty() {
                set_status(
                    &shared,
                    &format!(
                        "CAPTURE ERROR • {}",
                        tail.chars().take(120).collect::<String>()
                    ),
                );
            } else if bad_exit {
                if let Some(status) = exit {
                    set_status(&shared, &format!("CAPTURE ERROR • parec exited {status}"));
                }
            }
        }
    })
}

// ---------------------------------------------------------------------------
// stdin worker (Unix)
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn stdin_worker(mut input: File, shared: Arc<Mutex<Shared>>, stop: Arc<AtomicBool>) {
    let mut carry = [0u8; 4];
    let mut carry_len = 0usize;
    let mut chunk = [0u8; CHUNK];
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match poll_fd(&mut input, POLL_TIMEOUT) {
            PollState::Timeout => continue,
            PollState::Error(e) => {
                set_status(&shared, &format!("STDIN ERROR • {e}"));
                break;
            }
            PollState::Readable => match input.read(&mut chunk) {
                Ok(0) => {
                    let msg = if carry_len > 0 {
                        format!("STDIN CLOSED • {carry_len} trailing bytes dropped; bars decaying")
                    } else {
                        "STDIN CLOSED • EOF; bars decaying".to_string()
                    };
                    set_status(&shared, &msg);
                    break;
                }
                Ok(n) => push_samples(&shared, &mut carry, &mut carry_len, &chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    set_status(&shared, &format!("STDIN ERROR • {e}"));
                    break;
                }
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers (Unix)
// ---------------------------------------------------------------------------

/// Feed a byte chunk into the ring, reassembling partial 4-byte f32le samples
/// across reads. `Shared::push` sanitizes non-finite values.
#[cfg(unix)]
fn push_samples(
    shared: &Arc<Mutex<Shared>>,
    carry: &mut [u8; 4],
    carry_len: &mut usize,
    chunk: &[u8],
) {
    let mut buf = [0u8; CHUNK + 4];
    buf[..*carry_len].copy_from_slice(&carry[..*carry_len]);
    buf[*carry_len..*carry_len + chunk.len()].copy_from_slice(chunk);
    let total = *carry_len + chunk.len();
    let complete = total & !3;
    let count = complete / 4;
    let mut samples = [0f32; CHUNK / 4];
    for i in 0..count {
        samples[i] = f32::from_le_bytes(buf[i * 4..i * 4 + 4].try_into().unwrap());
    }
    if count > 0 {
        if let Ok(mut state) = shared.lock() {
            state.push(&samples[..count]);
        }
    }
    carry[..total - complete].copy_from_slice(&buf[complete..total]);
    *carry_len = total - complete;
}

#[cfg(unix)]
fn set_status(shared: &Arc<Mutex<Shared>>, status: &str) {
    if let Ok(mut state) = shared.lock() {
        if state.status != status {
            state.status = status.to_string();
        }
    }
}

/// Poll one fd for readability with a bounded timeout so `stop` is noticed.
#[cfg(unix)]
enum PollState {
    Readable,
    Timeout,
    Error(String),
}

#[cfg(unix)]
fn poll_fd<R>(stream: &mut R, timeout: Duration) -> PollState
where
    R: Read + std::os::unix::io::AsRawFd,
{
    let mut pfd = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = timeout.as_millis().min(libc::c_int::MAX as u128) as libc::c_int;
    let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
    if rc < 0 {
        let err = io::Error::last_os_error();
        // EINTR: a signal interrupted the wait; just poll again.
        if err.raw_os_error() == Some(libc::EINTR) {
            return PollState::Timeout;
        }
        return PollState::Error(err.to_string());
    }
    if rc == 0 {
        return PollState::Timeout;
    }
    // POLLIN, POLLHUP, POLLERR, POLLNVAL all fall through to a read, which
    // returns data, EOF (0), or an error — all handled by the caller.
    PollState::Readable
}

#[cfg(test)]
mod tests {
    use super::source_listing_line;

    #[test]
    fn regression_source_metadata_cannot_control_the_terminal() {
        let line = source_listing_line(
            7,
            "input\rforged",
            "Café\x1b]0;forged title\x07\nfake source",
        );
        assert!(!line.chars().any(char::is_control));
        assert!(line.contains("Café"));
        assert!(line.contains("forged title"));
    }
}
