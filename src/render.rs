use crate::{
    cli::{ColorMode, Palette},
    physics::{Personality, World, CELL_ASPECT, RADIUS},
};
use crossterm::{
    cursor, execute, queue,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal::{
        self, Clear, ClearType, DisableLineWrap, EnableLineWrap, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use std::{
    fmt::Write as FmtWrite,
    io::{self, Write},
    sync::atomic::{AtomicBool, Ordering},
};

static ACTIVE: AtomicBool = AtomicBool::new(false);
pub struct Terminal;
impl Terminal {
    pub fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        ACTIVE.store(true, Ordering::SeqCst);
        let guard = Self;
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            previous(info);
        }));
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            DisableLineWrap,
            cursor::Hide,
            Clear(ClearType::All)
        )?;
        Ok(guard)
    }
}
fn restore() {
    if ACTIVE.swap(false, Ordering::SeqCst) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            ResetColor,
            EnableLineWrap,
            cursor::Show,
            LeaveAlternateScreen
        );
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        restore();
    }
}

pub struct View {
    pub palette: Palette,
    pub sparks: bool,
    pub stars: bool,
    pub trails: bool,
    pub help: bool,
    pub paused: bool,
    pub time: f32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Cell {
    glyph: char,
    fg: Color,
    bg: Color,
}
const EMPTY: Cell = Cell {
    glyph: ' ',
    fg: Color::Reset,
    bg: Color::Reset,
};
const BLOCKS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// Fixed unit-circle samples for launch rings: no per-frame trig.
const RING_SAMPLES: [(f32, f32); 28] = [
    (1.0, 0.0),
    (0.974928, 0.222521),
    (0.900969, 0.433884),
    (0.781831, 0.623490),
    (0.623490, 0.781831),
    (0.433884, 0.900969),
    (0.222521, 0.974928),
    (0.0, 1.0),
    (-0.222521, 0.974928),
    (-0.433884, 0.900969),
    (-0.623490, 0.781831),
    (-0.781831, 0.623490),
    (-0.900969, 0.433884),
    (-0.974928, 0.222521),
    (-1.0, 0.0),
    (-0.974928, -0.222521),
    (-0.900969, -0.433884),
    (-0.781831, -0.623490),
    (-0.623490, -0.781831),
    (-0.433884, -0.900969),
    (-0.222521, -0.974928),
    (-0.0, -1.0),
    (0.222521, -0.974928),
    (0.433884, -0.900969),
    (0.623490, -0.781831),
    (0.781831, -0.623490),
    (0.900969, -0.433884),
    (0.974928, -0.222521),
];

pub struct Renderer {
    output: Vec<u8>,
    front: Vec<Cell>,
    back: Vec<Cell>,
    cols: u16,
    rows: u16,
    truecolor: bool,
    text: String,
}
impl Renderer {
    pub fn new(mode: ColorMode) -> Self {
        let truecolor = match mode {
            ColorMode::Truecolor => true,
            ColorMode::Ansi => false,
            ColorMode::Auto => {
                let hint = std::env::var("COLORTERM")
                    .unwrap_or_default()
                    .to_lowercase();
                let term = std::env::var("TERM").unwrap_or_default();
                std::env::var_os("NO_COLOR").is_none()
                    && (hint == "truecolor" || hint == "24bit" || term.ends_with("-direct"))
            }
        };
        // This app's mode selection owns color policy. Crossterm otherwise
        // suppresses even explicitly requested colors when NO_COLOR is set.
        crossterm::style::force_color_output(true);
        Self {
            output: Vec::with_capacity(128 * 1024),
            front: Vec::new(),
            back: Vec::new(),
            cols: 0,
            rows: 0,
            truecolor,
            text: String::with_capacity(512),
        }
    }

    fn put(&mut self, x: i32, y: i32, glyph: char, fg: Color) {
        if x >= 0 && y >= 0 && x < self.cols as i32 && y < self.rows as i32 {
            self.back[y as usize * self.cols as usize + x as usize] = Cell {
                glyph,
                fg,
                bg: Color::Reset,
            };
        }
    }

    fn label(&mut self, x: i32, y: i32, text: &str, color: Color) {
        for (i, ch) in text.chars().enumerate() {
            // Never forward terminal escapes or control bytes from device descriptions.
            self.put(
                x + i as i32,
                y,
                if ch.is_ascii() && !ch.is_control() {
                    ch
                } else {
                    '?'
                },
                color,
            );
        }
    }

    fn own_label(&mut self, x: i32, y: i32, color: Color) {
        for (i, ch) in self.text.chars().enumerate() {
            let column = x + i as i32;
            if column >= 0 && column < self.cols as i32 && y >= 0 && y < self.rows as i32 {
                self.back[y as usize * self.cols as usize + column as usize] = Cell {
                    glyph: if ch.is_ascii() && !ch.is_control() {
                        ch
                    } else {
                        '?'
                    },
                    fg: color,
                    bg: Color::Reset,
                };
            }
        }
    }

    fn tint(&self, palette: Palette, amount: f32) -> Color {
        let t = amount.clamp(0.0, 1.0);
        if !self.truecolor {
            return match palette {
                Palette::Aurora => {
                    if t < 0.5 {
                        Color::DarkCyan
                    } else {
                        Color::Cyan
                    }
                }
                Palette::Ember => {
                    if t < 0.45 {
                        Color::DarkRed
                    } else if t < 0.8 {
                        Color::Yellow
                    } else {
                        Color::White
                    }
                }
                Palette::Ocean => {
                    if t < 0.5 {
                        Color::DarkBlue
                    } else {
                        Color::Blue
                    }
                }
                Palette::Mono => {
                    if t < 0.5 {
                        Color::DarkGrey
                    } else {
                        Color::Grey
                    }
                }
            };
        }
        let (low, high) = match palette {
            Palette::Aurora => ([23.0, 65.0, 96.0], [101.0, 244.0, 190.0]),
            Palette::Ember => ([83.0, 31.0, 63.0], [255.0, 190.0, 104.0]),
            Palette::Ocean => ([25.0, 42.0, 91.0], [134.0, 185.0, 255.0]),
            Palette::Mono => ([49.0, 54.0, 65.0], [218.0, 227.0, 233.0]),
        };
        Color::Rgb {
            r: (low[0] + (high[0] - low[0]) * t) as u8,
            g: (low[1] + (high[1] - low[1]) * t) as u8,
            b: (low[2] + (high[2] - low[2]) * t) as u8,
        }
    }

    /// Per-personality ramp, coherent with the active palette. Each ball
    /// personality keeps the same identity across its sphere, trail and
    /// launch ring. Mono stays grayscale, with Floater brightest, Skimmer
    /// darkest and Balanced in between.
    fn personality_color(&self, palette: Palette, p: Personality, amount: f32) -> Color {
        let t = amount.clamp(0.0, 1.0);
        let (low, high, dark, light) = match (palette, p) {
            (Palette::Aurora, Personality::Floater) => (
                [20.0, 110.0, 80.0],
                [140.0, 255.0, 190.0],
                Color::DarkGreen,
                Color::Green,
            ),
            (Palette::Aurora, Personality::Skimmer) => (
                [10.0, 90.0, 120.0],
                [90.0, 220.0, 255.0],
                Color::DarkCyan,
                Color::Cyan,
            ),
            (Palette::Aurora, Personality::Balanced) => (
                [70.0, 45.0, 115.0],
                [205.0, 160.0, 255.0],
                Color::DarkMagenta,
                Color::Magenta,
            ),
            (Palette::Ember, Personality::Floater) => (
                [120.0, 60.0, 20.0],
                [255.0, 200.0, 90.0],
                Color::DarkYellow,
                Color::Yellow,
            ),
            (Palette::Ember, Personality::Skimmer) => (
                [110.0, 25.0, 40.0],
                [255.0, 90.0, 110.0],
                Color::DarkRed,
                Color::Red,
            ),
            (Palette::Ember, Personality::Balanced) => (
                [130.0, 45.0, 30.0],
                [255.0, 150.0, 70.0],
                Color::DarkYellow,
                Color::White,
            ),
            (Palette::Ocean, Personality::Floater) => (
                [40.0, 70.0, 160.0],
                [120.0, 190.0, 255.0],
                Color::DarkBlue,
                Color::Blue,
            ),
            (Palette::Ocean, Personality::Skimmer) => (
                [70.0, 40.0, 140.0],
                [170.0, 120.0, 255.0],
                Color::DarkMagenta,
                Color::Magenta,
            ),
            (Palette::Ocean, Personality::Balanced) => (
                [30.0, 90.0, 140.0],
                [110.0, 220.0, 230.0],
                Color::DarkBlue,
                Color::Cyan,
            ),
            (Palette::Mono, Personality::Floater) => {
                ([95.0; 3], [240.0; 3], Color::Grey, Color::White)
            }
            (Palette::Mono, Personality::Skimmer) => {
                ([48.0; 3], [145.0; 3], Color::Black, Color::DarkGrey)
            }
            (Palette::Mono, Personality::Balanced) => {
                ([75.0; 3], [195.0; 3], Color::DarkGrey, Color::Grey)
            }
        };
        if !self.truecolor {
            return if t < 0.55 { dark } else { light };
        }
        Color::Rgb {
            r: (low[0] + (high[0] - low[0]) * t) as u8,
            g: (low[1] + (high[1] - low[1]) * t) as u8,
            b: (low[2] + (high[2] - low[2]) * t) as u8,
        }
    }

    fn sphere_color(&self, palette: Palette, p: Personality, dx: f32, dy: f32, glow: f32) -> Color {
        let z = (1.0 - dx * dx - dy * dy).max(0.0).sqrt();
        // Impact glow brightens the head from the center out; it decays in
        // physics, so this is a brief local brighten, never a flash.
        let light = (0.35 - 0.25 * dx + 0.3 * dy + 0.45 * z + 0.45 * glow * z).clamp(0.0, 1.0);
        if light > 0.82 {
            // Highlight takes the personality's high color instead of a
            // shared near-white, so balls keep distinct identities.
            self.personality_color(palette, p, 1.0)
        } else if light > 0.65 {
            self.personality_color(palette, p, 0.85)
        } else {
            self.personality_color(palette, p, light)
        }
    }

    pub fn draw(
        &mut self,
        world: &World,
        cols: u16,
        rows: u16,
        status: &str,
        view: &View,
    ) -> io::Result<()> {
        self.output.clear();
        if cols != self.cols || rows != self.rows {
            self.cols = cols;
            self.rows = rows;
            self.front.resize(cols as usize * rows as usize, EMPTY);
            self.front.fill(EMPTY);
            self.back.resize(self.front.len(), EMPTY);
            queue!(self.output, ResetColor, Clear(ClearType::All))?;
        }
        self.back.fill(EMPTY);
        if cols < 24 || rows < 10 {
            self.label(0, 0, "Resize to 24x10", Color::Grey);
            if rows > 1 {
                self.label(0, 1, "Q / Esc to quit", Color::DarkGrey);
            }
        } else {
            let bottom = rows as i32 - 3;
            if view.stars {
                for y in 2..bottom {
                    for x in 0..cols as i32 {
                        let hash = (x as u32)
                            .wrapping_mul(374761393)
                            .wrapping_add((y as u32).wrapping_mul(668265263));
                        let hash = (hash ^ (hash >> 13)).wrapping_mul(1274126177);
                        if hash % 97 == 0 {
                            let color = if self.truecolor {
                                let shade = (65.0
                                    + 8.0 * (view.time * 0.35 + (hash % 71) as f32).sin())
                                    as u8;
                                Color::Rgb {
                                    r: shade,
                                    g: shade + 5,
                                    b: shade + 12,
                                }
                            } else {
                                Color::DarkGrey
                            };
                            let glyph = if hash % 9 == 0 { '+' } else { '·' };
                            self.put(x, y, glyph, color);
                        }
                    }
                }
            }
            for x in 0..cols {
                let h = World::height_at(&world.surface, world.width, x as f32 + 0.5) / CELL_ASPECT;
                for row in 0..h.ceil() as i32 {
                    let fraction = (h - row as f32).clamp(0.0, 1.0);
                    let glyph = BLOCKS[(fraction * 8.0).round() as usize];
                    let amount =
                        0.12 + 0.85 * row as f32 / (world.height * 0.57 / CELL_ASPECT).max(1.0);
                    self.put(
                        x as i32,
                        bottom - row,
                        glyph,
                        self.tint(view.palette, amount),
                    );
                }
            }
            // Launch rings: local, expanding (radius owned by physics),
            // fading with normalized life. Drawn above the bars, below the
            // balls, clipped to the playfield, and independent of the S
            // spark toggle. Fixed unit-circle samples avoid per-frame trig.
            for ring in &world.rings {
                let life = ring.life.clamp(0.0, 1.0);
                let color =
                    self.personality_color(view.palette, ring.personality, 0.25 + 0.65 * life);
                let glyph = if life > 0.5 { '•' } else { '·' };
                for &(ux, uy) in &RING_SAMPLES {
                    let px = ring.x + ux * ring.radius;
                    let py = ring.y + uy * ring.radius;
                    if px < 0.0 || px >= world.width || py <= 0.0 {
                        continue;
                    }
                    if py <= World::height_at(&world.surface, world.width, px) {
                        continue;
                    }
                    let x = px.floor() as i32;
                    let y = bottom - (py / CELL_ASPECT).floor() as i32;
                    if x < 0 || x >= cols as i32 || y < 1 || y > bottom {
                        continue;
                    }
                    self.put(x, y, glyph, color);
                }
            }
            if view.trails {
                for ball in &world.balls {
                    for age in (0..ball.trail_len).rev() {
                        let index =
                            (ball.trail_cursor + ball.trail.len() - 1 - age) % ball.trail.len();
                        let point = ball.trail[index];
                        if point.y > World::height_at(&world.surface, world.width, point.x) + RADIUS
                        {
                            let y = bottom - (point.y / CELL_ASPECT).floor() as i32;
                            self.put(
                                point.x.floor() as i32,
                                y,
                                if age < 6 {
                                    '•'
                                } else if age < 18 {
                                    '·'
                                } else {
                                    '.'
                                },
                                self.personality_color(
                                    view.palette,
                                    ball.personality,
                                    0.22 + 0.68
                                        * (1.0
                                            - age as f32 / ball.personality.trail_length() as f32)
                                        + 0.3
                                            * ball.impact_glow
                                            * (1.0 - age as f32 / 6.0).max(0.0),
                                ),
                            );
                        }
                    }
                }
            }
            if view.sparks {
                for spark in &world.sparks {
                    if spark.y > World::height_at(&world.surface, world.width, spark.x) {
                        self.put(
                            spark.x as i32,
                            bottom - (spark.y / CELL_ASPECT).floor() as i32,
                            if spark.life > 0.45 {
                                '*'
                            } else if spark.life > 0.2 {
                                '·'
                            } else {
                                '.'
                            },
                            if spark.life > 0.6 {
                                Color::White
                            } else {
                                self.tint(view.palette, (0.25 + spark.life).min(0.95))
                            },
                        );
                    }
                }
            }
            for ball in &world.balls {
                let rx = RADIUS * (1.0 + ball.squash * 0.6);
                let ry = RADIUS
                    * (1.0 - ball.squash * 0.38
                        + (ball.vy.abs() / 32.0).min(1.0) * 0.12 * (1.0 - ball.squash));
                let cy = ball.y - (RADIUS - ry);
                let glow = ball.impact_glow.clamp(0.0, 1.0);
                let left = (ball.x - rx).floor() as i32;
                let right = (ball.x + rx).ceil() as i32;
                let top = bottom - ((cy + ry) / CELL_ASPECT).floor() as i32;
                let foot = bottom - ((cy - ry) / CELL_ASPECT).floor() as i32;
                for y in top..=foot {
                    for x in left..=right {
                        if x < 0 || x >= cols as i32 || y < 1 || y > bottom {
                            continue;
                        }
                        let dx = (x as f32 + 0.5 - ball.x) / rx;
                        let sample = |offset: f32| {
                            let dy = ((bottom - y) as f32 * CELL_ASPECT + offset - cy) / ry;
                            if dx * dx + dy * dy <= 1.0 {
                                Some(self.sphere_color(
                                    view.palette,
                                    ball.personality,
                                    dx,
                                    dy,
                                    glow,
                                ))
                            } else {
                                None
                            }
                        };
                        let upper = sample(1.5);
                        let lower = sample(0.5);
                        let idx = y as usize * cols as usize + x as usize;
                        let underneath = self.back[idx];
                        let upper_bg = if underneath.glyph == '█' {
                            underneath.fg
                        } else {
                            underneath.bg
                        };
                        let lower_bg = if BLOCKS[1..].contains(&underneath.glyph) {
                            underneath.fg
                        } else {
                            underneath.bg
                        };
                        self.back[idx] = match (upper, lower) {
                            (Some(a), Some(b)) => Cell {
                                glyph: '▀',
                                fg: a,
                                bg: b,
                            },
                            (Some(a), None) => Cell {
                                glyph: '▀',
                                fg: a,
                                bg: lower_bg,
                            },
                            (None, Some(b)) => Cell {
                                glyph: '▄',
                                fg: b,
                                bg: upper_bg,
                            },
                            _ => underneath,
                        };
                    }
                }
            }
            self.text.clear();
            let _ = write!(
                self.text,
                "BEATFALL  {}  {}",
                world.balls.len(),
                if view.paused {
                    "PAUSED"
                } else {
                    view.palette.name()
                }
            );
            self.own_label(0, 0, self.tint(view.palette, 0.95));
            if cols >= 55 {
                self.label(cols as i32 - 19, 0, "[H] help   [Q] quit", Color::DarkGrey);
            }
            for x in 0..cols {
                self.put(
                    x as i32,
                    rows as i32 - 2,
                    '─',
                    self.tint(view.palette, 0.05),
                );
            }
            self.label(0, rows as i32 - 1, status, Color::DarkGrey);
            // A small, softly decaying indicator, never a full-screen flash.
            self.put(
                cols as i32 - 1,
                rows as i32 - 2,
                '●',
                self.tint(view.palette, 0.12 + 0.5 * world.beat_glow),
            );
            if view.help {
                let lines = [
                    " A/D or arrows  steer",
                    " Space          launch",
                    " +/-            balls (1-8)",
                    " P pause   R reset",
                    " T palette L trails",
                    " S sparks  B stars",
                    " H help    Q/Esc quit",
                ];
                let width = 28.min(cols as i32);
                let x = (cols as i32 - width) / 2;
                for (i, line) in lines.iter().enumerate() {
                    let y = 2 + i as i32;
                    if y >= rows as i32 - 2 {
                        break;
                    }
                    for column in x..x + width {
                        self.put(column, y, ' ', Color::Reset);
                    }
                    self.label(x, y, line, Color::Grey);
                }
            }
        }
        let mut cursor_pos = None;
        let mut fg = None;
        let mut bg = None;
        for (index, (cell, old)) in self.back.iter().zip(&self.front).enumerate() {
            if cell == old {
                continue;
            }
            let x = (index % cols as usize) as u16;
            let y = (index / cols as usize) as u16;
            if cursor_pos != Some((x, y)) {
                queue!(self.output, cursor::MoveTo(x, y))?;
            }
            if fg != Some(cell.fg) {
                queue!(self.output, SetForegroundColor(cell.fg))?;
                fg = Some(cell.fg);
            }
            if bg != Some(cell.bg) {
                queue!(self.output, SetBackgroundColor(cell.bg))?;
                bg = Some(cell.bg);
            }
            queue!(self.output, Print(cell.glyph))?;
            cursor_pos = Some((x + 1, y));
        }
        if !self.output.is_empty() {
            let mut stdout = io::stdout().lock();
            stdout.write_all(&self.output)?;
            stdout.flush()?;
        }
        std::mem::swap(&mut self.front, &mut self.back);
        Ok(())
    }
}
