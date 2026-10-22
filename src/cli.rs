use clap::{Parser, ValueEnum};

#[derive(Parser)]
#[command(
    version,
    about = "Live audio spectrum with bouncing balls",
    after_help = "Default: Linux system playback via PulseAudio/PipeWire-Pulse (pactl + parec).\nNative macOS/Windows capture is not implemented. --demo is synthetic and ignores\nsystem audio. Unix --stdin reads headerless f32le mono PCM; its producer must\nmatch --sample-rate. See README.md for audio setup and platform limitations.\n\nKeys: Q/Esc quit, A/D/arrows steer, Space launch, +/- balls, P pause,\n      T palette, S sparks, B stars, L trails, H help, R reset."
)]
pub struct Args {
    /// Generate synthetic audio; does not capture system sound or use speakers.
    #[arg(long, conflicts_with_all = ["stdin", "source", "microphone", "list_sources"])]
    pub demo: bool,
    /// Read raw little-endian 32-bit float mono PCM from stdin (Unix).
    #[arg(long, conflicts_with_all = ["demo", "source", "microphone", "list_sources"])]
    pub stdin: bool,
    /// Linux source name or numeric id; see --list-sources.
    #[arg(long, value_name = "NAME_OR_ID", conflicts_with_all = ["demo", "stdin", "list_sources"])]
    pub source: Option<String>,
    /// Capture an input/microphone instead of playback; rejects monitor sources.
    #[arg(long, conflicts_with_all = ["demo", "stdin", "list_sources"])]
    pub microphone: bool,
    /// List Linux playback monitors and microphone/input sources, then exit.
    #[arg(long, conflicts_with_all = ["demo", "stdin", "source", "microphone"])]
    pub list_sources: bool,
    /// PCM sample rate in Hz, 8000..192000; no automatic stdin resampling.
    #[arg(long, default_value_t = 48_000, value_parser = clap::value_parser!(u32).range(8000..=192000))]
    pub sample_rate: u32,
    /// Initial ball count, 1..8; cycles floater, skimmer, balanced.
    #[arg(long, default_value_t = 3, value_parser = balls)]
    pub balls: usize,
    /// Target rendering frames per second, 15..120; physics always runs at 240 Hz.
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u32).range(15..=120))]
    pub fps: u32,
    /// Spectrum gain, 0.05..20.
    #[arg(long, default_value_t = 1.0, value_parser = sensitivity)]
    pub sensitivity: f32,
    /// Downward acceleration in horizontal-cell widths/s^2, 5..150.
    #[arg(long, default_value_t = 38.0, value_parser = gravity)]
    pub gravity: f32,
    /// Initial spectrum and ball palette.
    #[arg(long, value_enum, default_value_t = Palette::Aurora)]
    pub palette: Palette,
    /// Auto-detect truecolor, force 24-bit RGB, or use the terminal's ANSI16 theme.
    #[arg(long, value_enum, default_value_t = ColorMode::Auto)]
    pub color: ColorMode,
    /// Enable fading ball trails at startup; L toggles them at runtime.
    #[arg(long)]
    pub trails: bool,
}

fn balls(text: &str) -> Result<usize, String> {
    let value = text
        .parse::<usize>()
        .map_err(|_| "expected an integer from 1 to 8")?;
    if (1..=8).contains(&value) {
        Ok(value)
    } else {
        Err("expected an integer from 1 to 8".into())
    }
}
fn bounded(text: &str, min: f32, max: f32) -> Result<f32, String> {
    let value = text
        .parse::<f32>()
        .map_err(|_| "expected a finite number")?;
    if value.is_finite() && (min..=max).contains(&value) {
        Ok(value)
    } else {
        Err(format!("expected a finite number from {min} to {max}"))
    }
}
fn sensitivity(text: &str) -> Result<f32, String> {
    bounded(text, 0.05, 20.0)
}
fn gravity(text: &str) -> Result<f32, String> {
    bounded(text, 5.0, 150.0)
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Palette {
    Aurora,
    Ember,
    Ocean,
    Mono,
}
impl Palette {
    pub fn next(self) -> Self {
        match self {
            Self::Aurora => Self::Ember,
            Self::Ember => Self::Ocean,
            Self::Ocean => Self::Mono,
            Self::Mono => Self::Aurora,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Aurora => "aurora",
            Self::Ember => "ember",
            Self::Ocean => "ocean",
            Self::Mono => "mono",
        }
    }
}
#[derive(Clone, Copy, ValueEnum)]
pub enum ColorMode {
    Auto,
    Truecolor,
    Ansi,
}
