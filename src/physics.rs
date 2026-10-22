use crate::spectrum::BANDS;

pub const STEP: f32 = 1.0 / 240.0;
/// Physics uses horizontal-cell widths; a terminal row is twice as tall.
pub const CELL_ASPECT: f32 = 2.0;
pub const RADIUS: f32 = 1.25;
const TRAIL_LENGTH: usize = 30;
const MAX_SPARKS: usize = 256;
const MAX_RINGS: usize = 24;
// Even at this cap a substep moves less than one radius. Headroom imposes
// a tighter gravity-aware limit, so large/high-gravity views can still bounce.
const MAX_RISE_SPEED: f32 = 80.0;
/// Minimum bounce reaches this fraction of the free height above the contact point.
const MIN_BOUNCE_FRACTION: f32 = 0.10;
const RESTITUTION: f32 = 0.62;
/// Repeated landings confined to a 16-cell region trigger an escape hop.
const TRAP_LIMIT: f32 = 2.5;
const RING_LIFE: f32 = 0.55;
const RING_GROWTH: f32 = 14.0;
const JUMP_COOLDOWN: f32 = 0.3;
const BOOST_COOLDOWN: f32 = 0.4;
const GLOW_DECAY: f32 = 0.3;

#[derive(Clone, Copy, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// Motion archetypes: floater drifts slow and high, skimmer skims fast and low,
/// balanced sits between.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Personality {
    Floater,
    Skimmer,
    Balanced,
}

impl Personality {
    pub fn trail_length(self) -> usize {
        match self {
            Personality::Floater => 30,
            Personality::Skimmer => 15,
            Personality::Balanced => 22,
        }
    }

    const fn cruise_speed(self) -> f32 {
        match self {
            Personality::Floater => 5.5,
            Personality::Skimmer => 13.0,
            Personality::Balanced => 8.5,
        }
    }

    const fn bounce_scale(self) -> f32 {
        match self {
            Personality::Floater => 1.3,
            Personality::Skimmer => 0.72,
            Personality::Balanced => 1.0,
        }
    }

    const fn launch_scale(self) -> f32 {
        match self {
            Personality::Floater => 1.15,
            Personality::Skimmer => 0.8,
            Personality::Balanced => 1.0,
        }
    }
}

const PERSONALITIES: [Personality; 3] = [
    Personality::Floater,
    Personality::Skimmer,
    Personality::Balanced,
];

pub struct Ball {
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub squash: f32,
    pub personality: Personality,
    /// Normalized 0..1 impact flash, decaying over ~0.3s.
    pub impact_glow: f32,
    pub trail: [Point; TRAIL_LENGTH],
    pub trail_len: usize,
    pub trail_cursor: usize,
    cruise_dir: f32,
    trapped_time: f32,
    anchor_x: f32,
    landing_count: u32,
    escape_height: f32,
    boosted_this_flight: bool,
    boost_cooldown: f32,
    jump_cooldown: f32,
}

pub struct Spark {
    pub x: f32,
    pub y: f32,
    vx: f32,
    vy: f32,
    pub life: f32,
}

/// Launch ring from an accepted Space press. `life` is normalized 1..0.
pub struct Ring {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    pub life: f32,
    pub personality: Personality,
}

/// Highest circle-support contact over the local spectrum footprint. Position,
/// normal and sampled surface velocity refer to this same point.
struct Contact {
    px: f32,
    py: f32,
    nx: f32,
    ny: f32,
    top: f32,
}

pub struct World {
    pub balls: Vec<Ball>,
    pub surface: [f32; BANDS],
    target: [f32; BANDS],
    velocity: [f32; BANDS],
    pub sparks: Vec<Spark>,
    pub rings: Vec<Ring>,
    pub width: f32,
    pub height: f32,
    gravity: f32,
    trail_clock: f32,
    random: u32,
    pub beat_glow: f32,
}

impl World {
    pub fn new(width: f32, height: f32, gravity: f32, count: usize) -> Self {
        let mut world = Self {
            balls: Vec::with_capacity(8),
            surface: [1.0; BANDS],
            target: [1.0; BANDS],
            velocity: [0.0; BANDS],
            sparks: Vec::with_capacity(MAX_SPARKS),
            rings: Vec::with_capacity(MAX_RINGS),
            width: width.max(20.0),
            height: height.max(12.0),
            gravity,
            trail_clock: 0.0,
            random: 0x1234_abcd,
            beat_glow: 0.0,
        };
        world.reset(count);
        world
    }

    pub fn reset(&mut self, count: usize) {
        self.balls.clear();
        self.sparks.clear();
        self.rings.clear();
        for i in 0..count.clamp(1, 8) {
            self.spawn(self.width * (i as f32 + 1.0) / (count.clamp(1, 8) + 1) as f32);
        }
    }

    fn spawn(&mut self, x: f32) {
        let i = self.balls.len();
        let dir = if i % 2 == 0 { 1.0 } else { -1.0 };
        let x = x.clamp(RADIUS, self.width - RADIUS);
        self.balls.push(Ball {
            x,
            y: self.height * 0.85,
            vx: dir * 7.0,
            vy: 0.0,
            squash: 0.0,
            personality: PERSONALITIES[i % PERSONALITIES.len()],
            impact_glow: 0.0,
            trail: [Point::default(); TRAIL_LENGTH],
            trail_len: 0,
            trail_cursor: 0,
            cruise_dir: dir,
            trapped_time: 0.0,
            anchor_x: x,
            landing_count: 0,
            escape_height: 0.0,
            boosted_this_flight: false,
            boost_cooldown: 0.0,
            jump_cooldown: 0.0,
        });
    }

    pub fn add_ball(&mut self) {
        if self.balls.len() < 8 {
            self.spawn(self.width * (0.2 + 0.6 * self.balls.len() as f32 / 7.0));
        }
    }

    pub fn remove_ball(&mut self) {
        if self.balls.len() > 1 {
            self.balls.pop();
        }
    }

    pub fn resize(&mut self, width: f32, height: f32) {
        let width = width.max(20.0);
        let height = height.max(12.0);
        let sx = width / self.width;
        let sy = height / self.height;
        for ball in &mut self.balls {
            ball.x = (ball.x * sx).clamp(RADIUS, width - RADIUS);
            ball.y = (ball.y * sy).clamp(RADIUS, height - RADIUS);
            ball.trail_len = 0;
            // A coordinate change is not an impact: clear transient tracking.
            ball.trapped_time = 0.0;
            ball.anchor_x = ball.x;
            ball.landing_count = 0;
            ball.escape_height = 0.0;
            ball.squash = 0.0;
            ball.impact_glow = 0.0;
            ball.boosted_this_flight = false;
            ball.boost_cooldown = 0.0;
            ball.jump_cooldown = 0.0;
        }
        for i in 0..BANDS {
            self.surface[i] *= sy;
            self.target[i] *= sy;
        }
        self.velocity.fill(0.0);
        self.sparks.clear();
        self.rings.clear();
        self.width = width;
        self.height = height;
        // Resizing is a coordinate change, not an impact: avoid spurious impulses.
        for ball in &mut self.balls {
            ball.y = ball.y.max(Self::support(&self.surface, width, ball.x));
        }
    }

    pub fn set_surface(&mut self, bands: &[f32]) {
        for (height, value) in self.target.iter_mut().zip(bands) {
            *height = 1.0 + value.clamp(0.0, 1.0) * self.height * 0.57;
        }
    }

    pub fn height_at(surface: &[f32], width: f32, x: f32) -> f32 {
        let u = (x / (width - 1.0).max(1.0) * (surface.len() - 1) as f32)
            .clamp(0.0, (surface.len() - 1) as f32);
        let i = u.floor() as usize;
        let next = (i + 1).min(surface.len() - 1);
        surface[i] + (surface[next] - surface[i]) * u.fract()
    }

    /// Maximize surface(px) + sqrt(radius² - (px - x)²) over each intersecting
    /// segment. Its stationary point or a segment endpoint is the exact support;
    /// picking the highest prevents resolving one wall into another in a valley.
    fn footprint(surface: &[f32], width: f32, x: f32) -> Contact {
        let spacing = (width - 1.0).max(1.0) / (surface.len() - 1) as f32;
        let py = Self::height_at(surface, width, x);
        let mut best = Contact {
            px: x,
            py,
            nx: 0.0,
            ny: 1.0,
            top: py + RADIUS,
        };
        let first = (((x - RADIUS) / spacing).floor().max(0.0) as usize).min(surface.len() - 2);
        let last = (((x + RADIUS) / spacing).floor().max(0.0) as usize).min(surface.len() - 2);
        for i in first..=last {
            let ax = i as f32 * spacing;
            let slope = (surface[i + 1] - surface[i]) / spacing;
            let offset = RADIUS * slope / (1.0 + slope * slope).sqrt();
            let px = (x + offset).clamp(ax.max(x - RADIUS), (ax + spacing).min(x + RADIUS));
            let dx = x - px;
            let dy = (RADIUS * RADIUS - dx * dx).max(0.0).sqrt();
            let py = surface[i] + slope * (px - ax);
            if py + dy > best.top {
                best = Contact {
                    px,
                    py,
                    nx: dx / RADIUS,
                    ny: dy / RADIUS,
                    top: py + dy,
                };
            }
        }
        best
    }

    fn support(surface: &[f32], width: f32, x: f32) -> f32 {
        Self::footprint(surface, width, x).top
    }

    fn contact(surface: &[f32], width: f32, x: f32, y: f32) -> Option<Contact> {
        let contact = Self::footprint(surface, width, x);
        (y <= contact.top).then_some(contact)
    }

    /// Clearance for the whole escape corridor, including narrow intermediate peaks.
    fn barrier(surface: &[f32], width: f32, from: f32, to: f32) -> f32 {
        let span = (width - 1.0).max(1.0);
        let spacing = span / (surface.len() - 1) as f32;
        let low = (from.min(to) - RADIUS).max(0.0);
        let high = (from.max(to) + RADIUS).min(span);
        let mut top =
            Self::height_at(surface, width, low).max(Self::height_at(surface, width, high));
        let first = (low / spacing).ceil() as usize;
        let last = ((high / spacing).floor() as usize).min(surface.len() - 1);
        if first <= last {
            for &height in &surface[first..=last] {
                top = top.max(height);
            }
        }
        top + RADIUS
    }

    /// Normalized 0..1 audio energy under x, from the current per-band target heights.
    fn local_energy(target: &[f32], width: f32, height: f32, x: f32) -> f32 {
        ((Self::height_at(target, width, x) - 1.0) / (height * 0.57)).clamp(0.0, 1.0)
    }

    /// Called only with STEP. Bounded speeds keep each substep below one ball radius.
    pub fn step(&mut self, steer: f32, beat: bool, sparks: bool, trails: bool) {
        let dt = STEP;
        for i in 0..BANDS {
            let delta = ((self.target[i] - self.surface[i]) * (1.0 - (-dt / 0.04).exp()))
                .clamp(-60.0 * dt, 60.0 * dt);
            self.surface[i] += delta;
            self.velocity[i] = delta / dt;
        }
        self.beat_glow = (self.beat_glow - dt * 2.5).max(0.0);
        if beat {
            self.beat_glow = 1.0;
        }
        self.trail_clock += dt;
        let record_trail = self.trail_clock >= 1.0 / 30.0;
        if record_trail {
            self.trail_clock -= 1.0 / 30.0;
        }
        for ball in &mut self.balls {
            ball.squash = (ball.squash - dt * 7.0).max(0.0);
            ball.impact_glow = (ball.impact_glow - dt / GLOW_DECAY).max(0.0);
            ball.jump_cooldown = (ball.jump_cooldown - dt).max(0.0);
            ball.boost_cooldown = (ball.boost_cooldown - dt).max(0.0);
            if steer != 0.0 {
                // Manual steering overrides the autonomous cruise and remembers its direction.
                ball.cruise_dir = steer.signum();
                ball.escape_height = 0.0;
                ball.vx = (ball.vx + steer * 38.0 * dt).clamp(-24.0, 24.0);
            } else if ball.escape_height > 0.0 {
                // Clear the obstacle before moving sideways; otherwise a steep
                // side collision immediately reflects the escape back into its trap.
                if ball.y < ball.escape_height && ball.vy > 0.0 {
                    ball.vx = 0.0;
                } else {
                    ball.vx = ball.cruise_dir * 18.0;
                    ball.escape_height = 0.0;
                }
            } else {
                let cruise = ball.cruise_dir * ball.personality.cruise_speed();
                ball.vx += (cruise - ball.vx) * (1.0 - (-dt / 0.9).exp());
            }
            ball.vx *= (-0.035 * dt).exp();
            ball.vy = (ball.vy - self.gravity * dt).max(-80.0);
            ball.x += ball.vx * dt;
            ball.y += ball.vy * dt;

            // Resolve walls before sampling terrain so a boundary clamp cannot
            // leave the footprint intersecting a different part of the surface.
            if ball.x < RADIUS {
                ball.x = RADIUS;
                ball.vx = ball.vx.abs() * 0.85;
                ball.cruise_dir = 1.0;
            }
            if ball.x > self.width - RADIUS {
                ball.x = self.width - RADIUS;
                ball.vx = -ball.vx.abs() * 0.85;
                ball.cruise_dir = -1.0;
            }
            let mut landed = false;
            if let Some(c) = Self::contact(&self.surface, self.width, ball.x, ball.y) {
                landed = true;
                ball.y = c.top + 0.01;
                let surface_v = Self::height_at(&self.velocity, self.width, c.px);
                let rvx = ball.vx;
                let rvy = ball.vy - surface_v;
                let vn = rvx * c.nx + rvy * c.ny;
                if vn < 0.0 {
                    let impact = -vn;
                    let p = ball.personality;
                    let headroom = (self.height - RADIUS - ball.y).max(0.0);
                    let energy = Self::local_energy(&self.target, self.width, self.height, c.px);
                    let bounce_height =
                        headroom * (MIN_BOUNCE_FRACTION + energy * 0.18) * p.bounce_scale().powi(2);
                    let minimum = (2.0 * self.gravity * bounce_height).sqrt();
                    let ceiling = (2.0 * self.gravity * headroom * 0.85)
                        .sqrt()
                        .min(MAX_RISE_SPEED);
                    let rebound = impact * RESTITUTION;
                    let tx = (rvx - vn * c.nx) * 0.985;
                    let ty = (rvy - vn * c.ny) * 0.985;
                    ball.vx = (tx + c.nx * rebound).clamp(-24.0, 24.0);
                    // Floor the vertical launch, not just its normal component:
                    // a near-vertical wall must not turn every bounce sideways.
                    ball.vy = (ty + c.ny * rebound + surface_v).max(minimum).min(ceiling);
                    ball.squash = (impact / 24.0).clamp(0.0, 1.0);
                    ball.impact_glow = (impact / 16.0).min(1.0);
                    ball.boosted_this_flight = false;
                    if sparks && impact > 7.0 {
                        let count = (3.0 + impact * 0.45).min(14.0) as usize;
                        let speed = 1.0 + impact / 50.0;
                        for _ in 0..count {
                            if self.sparks.len() >= MAX_SPARKS {
                                break;
                            }
                            self.random ^= self.random << 13;
                            self.random ^= self.random >> 17;
                            self.random ^= self.random << 5;
                            let r = (self.random & 0xffff) as f32 / 65535.0;
                            self.sparks.push(Spark {
                                x: c.px,
                                y: c.py + RADIUS * 0.5,
                                vx: (r - 0.5) * 26.0 * speed,
                                vy: (11.0 + (1.0 - (2.0 * r - 1.0).abs()) * 8.0) * speed,
                                life: 0.4 + r * 0.3 + impact * 0.004,
                            });
                        }
                    }
                    ball.landing_count = ball.landing_count.saturating_add(1);
                }
            }

            if ball.y > self.height - RADIUS {
                ball.y = self.height - RADIUS;
                ball.vy = -ball.vy.abs() * 0.65;
            }

            // A beat onset is a visible strong launch: at most once per flight,
            // gated by headroom and cooldown so repeated beats cannot roof-pin.
            if beat && !ball.boosted_this_flight && ball.boost_cooldown <= 0.0 {
                let headroom = (self.height - RADIUS - ball.y).max(0.0);
                if headroom > 4.0 {
                    let p = ball.personality;
                    let energy = Self::local_energy(&self.target, self.width, self.height, ball.x);
                    let launch = (2.0 * self.gravity * headroom * 0.5 * p.launch_scale()).sqrt()
                        * (0.75 + 0.5 * energy);
                    let cap = (2.0 * self.gravity * headroom * 0.85)
                        .sqrt()
                        .min(MAX_RISE_SPEED);
                    ball.vy = ball.vy.max(launch.min(cap));
                    ball.boosted_this_flight = true;
                    ball.boost_cooldown = BOOST_COOLDOWN;
                }
            }

            // Detect repeated confined landings, not an ordinary airborne apex.
            if (ball.x - ball.anchor_x).abs() > 8.0 || steer != 0.0 {
                ball.anchor_x = ball.x;
                ball.trapped_time = 0.0;
                ball.landing_count = 0;
            } else {
                ball.trapped_time += dt;
            }
            if ball.trapped_time > TRAP_LIMIT && ball.landing_count >= 3 && landed && steer == 0.0 {
                let look = 6.0_f32.min(self.width * 0.2);
                let left = Self::barrier(
                    &self.surface,
                    self.width,
                    ball.x,
                    (ball.x - look).max(RADIUS),
                );
                let right = Self::barrier(
                    &self.surface,
                    self.width,
                    ball.x,
                    (ball.x + look).min(self.width - RADIUS),
                );
                if ball.x < RADIUS + look {
                    ball.cruise_dir = 1.0;
                } else if ball.x > self.width - RADIUS - look || left + 1.0 < right {
                    ball.cruise_dir = -1.0;
                } else if right + 1.0 < left {
                    ball.cruise_dir = 1.0;
                }
                let headroom = (self.height - RADIUS - ball.y).max(0.0);
                let obstacle = if ball.cruise_dir < 0.0 { left } else { right };
                let rise = (obstacle - ball.y + RADIUS * 2.0)
                    .max(headroom * 0.55)
                    .min(headroom * 0.8);
                let hop = (2.0 * self.gravity * rise).sqrt().min(MAX_RISE_SPEED);
                ball.vy = ball.vy.max(hop);
                ball.escape_height = (obstacle + 0.25).min(ball.y + rise * 0.9);
                ball.vx = 0.0;
                ball.boosted_this_flight = true;
                ball.trapped_time = 0.0;
                ball.anchor_x = ball.x;
                ball.landing_count = 0;
            }

            if !trails {
                ball.trail_len = 0;
            } else if record_trail {
                ball.trail[ball.trail_cursor] = Point {
                    x: ball.x,
                    y: ball.y,
                };
                ball.trail_cursor = (ball.trail_cursor + 1) % TRAIL_LENGTH;
                ball.trail_len = (ball.trail_len + 1).min(ball.personality.trail_length());
            }
        }
        if !sparks {
            self.sparks.clear();
        }
        for spark in &mut self.sparks {
            spark.life -= dt;
            spark.vy -= self.gravity * 0.55 * dt;
            spark.x += spark.vx * dt;
            spark.y += spark.vy * dt;
        }
        self.sparks
            .retain(|spark| spark.life > 0.0 && spark.y > 0.0);
        for ring in &mut self.rings {
            ring.radius += RING_GROWTH * dt;
            ring.life -= dt / RING_LIFE;
        }
        self.rings.retain(|ring| ring.life > 0.0);
    }

    pub fn jump(&mut self) {
        for ball in &mut self.balls {
            if ball.jump_cooldown > 0.0 {
                continue;
            }
            let headroom = (self.height - RADIUS - ball.y).max(0.0);
            if headroom < 3.0 {
                continue;
            }
            let p = ball.personality;
            let launch = (2.0 * self.gravity * headroom * 0.45 * p.launch_scale()).sqrt();
            let cap = (2.0 * self.gravity * headroom * 0.8)
                .sqrt()
                .min(MAX_RISE_SPEED);
            let launch = launch.min(cap);
            if ball.vy >= launch {
                continue;
            }
            ball.vy = launch;
            ball.jump_cooldown = JUMP_COOLDOWN;
            ball.boosted_this_flight = true;
            if self.rings.len() < MAX_RINGS {
                self.rings.push(Ring {
                    x: ball.x,
                    y: ball.y - RADIUS,
                    radius: RADIUS,
                    life: 1.0,
                    personality: p,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat() -> World {
        let mut world = World::new(100.0, 50.0, 38.0, 1);
        world.surface.fill(10.0);
        world.target.fill(10.0);
        world.balls[0].x = 50.0;
        world.balls[0].vx = 0.0;
        world
    }

    #[test]
    fn fast_fall_bounces_without_tunneling() {
        let mut world = flat();
        world.balls[0].y = 10.0 + RADIUS + 0.1;
        world.balls[0].vy = -60.0;
        world.step(0.0, false, true, false);
        let ball = &world.balls[0];
        assert!(ball.y >= 10.0 + RADIUS);
        assert!(ball.vy > 0.0);
        assert!(ball.squash > 0.0);
    }

    #[test]
    fn slope_deflects_falling_ball_downhill() {
        let mut world = flat();
        for i in 0..BANDS {
            world.surface[i] = 5.0 + i as f32 * 0.15;
        }
        world.target = world.surface;
        world.balls[0].y = World::support(&world.surface, world.width, 50.0) + 0.01;
        world.balls[0].vy = -12.0;
        world.step(0.0, false, false, false);
        assert!(world.balls[0].vx < -1.0);
        assert!(world.balls[0].vy > 0.0);
    }

    #[test]
    fn rising_surface_releases_embedded_ball_with_bounded_impulse() {
        let mut world = flat();
        world.target.fill(25.0);
        world.balls[0].y = 7.0;
        for _ in 0..240 {
            world.step(0.0, false, false, false);
            let b = &world.balls[0];
            assert!(b.y >= World::support(&world.surface, world.width, b.x));
            assert!(b.vy.abs() * STEP < RADIUS);
            assert!(b.x.is_finite() && b.y.is_finite());
        }
        assert!(world.balls[0].y > 25.0);
    }

    #[test]
    fn moving_surface_transfers_more_energy_than_static_surface() {
        let mut still = flat();
        let mut rising = flat();
        for w in [&mut still, &mut rising] {
            w.balls[0].y = 10.0 + RADIUS;
            w.balls[0].vy = -8.0;
        }
        rising.target.fill(20.0);
        still.step(0.0, false, false, false);
        rising.step(0.0, false, false, false);
        assert!(rising.balls[0].vy > still.balls[0].vy);
    }

    #[test]
    fn bounce_has_airtime_and_horizontal_travel() {
        let mut world = flat();
        world.balls[0].y = 10.0 + RADIUS;
        world.balls[0].vy = -16.0;
        world.balls[0].vx = 8.0;
        for _ in 0..60 {
            world.step(0.0, false, false, false);
        }
        assert!(world.balls[0].y > 10.0 + RADIUS + 1.0);
        assert!(world.balls[0].x > 51.0);
    }

    #[test]
    fn regression_rising_bar_impacts_an_upward_moving_ball() {
        let mut world = flat();
        world.target.fill(25.0);
        world.balls[0].y = 10.0 + RADIUS;
        world.balls[0].vy = 20.0;
        world.step(0.0, false, true, false);
        assert!(
            world.balls[0].vy > 20.0,
            "the faster rising bar must transfer upward momentum"
        );
        assert!(world.balls[0].squash > 0.0);
    }

    #[test]
    fn resizing_and_ball_limits_remain_stable() {
        let mut world = flat();
        for _ in 0..12 {
            world.add_ball();
        }
        assert_eq!(world.balls.len(), 8);
        world.resize(2.0, 1.0);
        for _ in 0..1000 {
            world.step(1.0, true, true, true);
        }
        for b in &world.balls {
            assert!((RADIUS..=world.width - RADIUS).contains(&b.x));
            assert!(b.y.is_finite() && b.y <= world.height - RADIUS);
        }
        assert!(world.sparks.len() <= MAX_SPARKS);
        for _ in 0..12 {
            world.remove_ball();
        }
        assert_eq!(world.balls.len(), 1);
    }

    #[test]
    fn spawn_cycles_personalities_and_reset_restarts() {
        let mut world = World::new(100.0, 50.0, 38.0, 3);
        assert_eq!(world.balls[0].personality, Personality::Floater);
        assert_eq!(world.balls[1].personality, Personality::Skimmer);
        assert_eq!(world.balls[2].personality, Personality::Balanced);
        world.add_ball();
        assert_eq!(world.balls[3].personality, Personality::Floater);
        world.remove_ball();
        assert_eq!(world.balls.len(), 3);
        world.reset(1);
        assert_eq!(world.balls[0].personality, Personality::Floater);
    }

    #[test]
    fn personalities_have_distinct_cruise_speeds() {
        let mut world = World::new(100.0, 50.0, 38.0, 3);
        for _ in 0..480 {
            world.step(0.0, false, false, false);
        }
        let v0 = world.balls[0].vx.abs();
        let v1 = world.balls[1].vx.abs();
        let v2 = world.balls[2].vx.abs();
        assert!(
            v1 > v2 && v2 > v0,
            "skimmer {} > balanced {} > floater {}",
            v1,
            v2,
            v0
        );
    }

    #[test]
    fn disabling_trails_discards_history_before_reenabling() {
        let mut world = World::new(100.0, 50.0, 38.0, 3);
        for _ in 0..600 {
            world.step(0.0, false, false, true);
        }
        assert!(world.balls[0].trail_len > world.balls[2].trail_len);
        assert!(world.balls[2].trail_len > world.balls[1].trail_len);
        world.step(0.0, false, false, false);
        assert!(world.balls.iter().all(|b| b.trail_len == 0));
        for _ in 0..8 {
            world.step(0.0, false, false, true);
        }
        assert!(world.balls.iter().all(|b| b.trail_len == 1));
    }

    #[test]
    fn quiet_static_terrain_keeps_bouncing_and_traversing() {
        let mut world = World::new(100.0, 50.0, 38.0, 3);
        for _ in 0..2400 {
            world.step(0.0, false, false, false);
        }
        let mut low = [f32::INFINITY; 3];
        let mut high = [f32::NEG_INFINITY; 3];
        let mut left = [f32::INFINITY; 3];
        let mut right = [f32::NEG_INFINITY; 3];
        for _ in 0..2400 {
            world.step(0.0, false, false, false);
            for (i, b) in world.balls.iter().enumerate() {
                low[i] = low[i].min(b.y);
                high[i] = high[i].max(b.y);
                left[i] = left[i].min(b.x);
                right[i] = right[i].max(b.x);
            }
        }
        for i in 0..3 {
            assert!(high[i] - low[i] > 1.0, "ball {i} stopped bouncing");
            assert!(right[i] - left[i] > 15.0, "ball {i} stopped traversing");
        }
        assert!(high[0] > high[2] && high[2] > high[1]);
    }

    #[test]
    fn steering_overrides_cruise_and_remembers_direction() {
        let mut world = flat();
        for _ in 0..120 {
            world.step(-1.0, false, false, false);
        }
        for _ in 0..300 {
            world.step(0.0, false, false, false);
        }
        assert!(
            world.balls[0].vx < -2.0,
            "cruise must continue in the steered direction: {}",
            world.balls[0].vx
        );
    }

    #[test]
    fn narrow_valley_escape() {
        let mut world = flat();
        // A box-shaped valley: flat bottom [44, 56] at 10, walls rising to 18,
        // flat at 18 beyond. The minimum bounce (apex ~6.8) cannot clear the
        // 8-deep walls, so only the trap hop can get the ball out.
        for i in 0..BANDS {
            let x = i as f32 / (BANDS - 1) as f32 * (world.width - 1.0);
            world.surface[i] = if x < 40.0 || x > 60.0 {
                18.0
            } else if x < 44.0 {
                18.0 - 8.0 * (x - 40.0) / 4.0
            } else if x > 56.0 {
                10.0 + 8.0 * (x - 56.0) / 4.0
            } else {
                10.0
            };
        }
        world.target = world.surface;
        world.balls[0].x = 50.0;
        world.balls[0].y = World::support(&world.surface, world.width, 50.0) + 0.01;
        world.balls[0].vx = 0.0;
        world.balls[0].vy = 0.0;
        let mut escaped = false;
        for _ in 0..3000 {
            world.step(0.0, false, false, false);
            let b = &world.balls[0];
            assert!(b.x.is_finite() && b.y.is_finite());
            assert!(b.y >= World::height_at(&world.surface, world.width, b.x) - 0.5);
            if b.x > 60.0 || b.x < 40.0 {
                escaped = true;
                break;
            }
        }
        assert!(escaped, "ball must hop out of the narrow valley");
    }

    #[test]
    fn narrow_peak_footprint_and_escape() {
        let mut world = flat();
        world.surface[32] = 12.0;
        world.target[32] = 12.0;
        world.balls[0].x = 50.0;
        world.balls[0].y = 10.0 + RADIUS;
        world.balls[0].vx = 0.0;
        world.balls[0].vy = 0.0;
        world.step(0.0, false, false, false);
        assert!(
            World::contact(
                &world.surface,
                world.width,
                world.balls[0].x,
                world.balls[0].y
            )
            .is_none(),
            "no penetration through the spike: y={}",
            world.balls[0].y
        );
        let mut clear = false;
        for _ in 0..1200 {
            world.step(0.0, false, false, false);
            if (world.balls[0].x - 50.0).abs() > 4.0 {
                clear = true;
                break;
            }
        }
        assert!(clear, "ball must leave the narrow peak");
    }

    #[test]
    fn beat_launches_airborne_ball_once_per_flight() {
        let mut world = flat();
        world.balls[0].y = 25.0;
        world.balls[0].vy = 0.0;
        world.balls[0].vx = 0.0;
        world.step(0.0, true, false, false);
        let first = world.balls[0].vy;
        assert!(
            first > 10.0,
            "beat must be a visible strong launch: {}",
            first
        );
        for _ in 0..80 {
            world.step(0.0, true, false, false);
            assert!(
                world.balls[0].vy <= first + 0.01,
                "no re-boost mid-flight: {}",
                world.balls[0].vy
            );
        }
    }

    #[test]
    fn repeated_beats_cannot_roof_pin() {
        for gravity in [5.0, 38.0, 150.0] {
            let mut world = World::new(24.0, 12.0, gravity, 3);
            let bands =
                std::array::from_fn::<_, BANDS, _>(|i| if (i / 3) % 2 == 0 { 0.85 } else { 0.0 });
            world.set_surface(&bands);
            let mut roof_frames = 0;
            for tick in 0..7200 {
                world.step(0.0, tick % 60 == 0, false, false);
                for b in &world.balls {
                    assert!(b.y <= world.height - RADIUS);
                    if tick > 2400 && b.y > world.height - RADIUS - 0.3 {
                        roof_frames += 1;
                    }
                }
            }
            assert!(roof_frames < 100, "ceiling pinning at gravity {gravity}");
        }
    }

    #[test]
    fn space_launch_is_bounded_cooldown_gated_and_emits_rings() {
        let mut world = flat();
        world.balls[0].y = 10.0 + RADIUS;
        world.balls[0].vy = 0.0;
        world.jump();
        let vy = world.balls[0].vy;
        assert!(vy > 0.0 && vy * STEP < RADIUS);
        assert_eq!(world.rings.len(), 1);
        assert_eq!(world.rings[0].personality, world.balls[0].personality);
        assert_eq!(world.rings[0].life, 1.0);
        // Immediate re-press: cooldown, no second ring.
        world.jump();
        assert_eq!(world.rings.len(), 1);
        // After the cooldown, an accepted launch emits another ring.
        for _ in 0..96 {
            world.step(0.0, false, false, false);
        }
        world.jump();
        assert_eq!(world.rings.len(), 2);
        // No headroom: rejected, no ring.
        for _ in 0..72 {
            world.step(0.0, false, false, false);
        }
        world.balls[0].y = world.height - RADIUS - 0.5;
        world.balls[0].vy = 0.0;
        let before = world.rings.len();
        world.jump();
        assert_eq!(world.rings.len(), before);
    }

    #[test]
    fn rings_grow_and_expire() {
        let mut world = flat();
        world.balls[0].y = 10.0 + RADIUS;
        world.jump();
        let r0 = world.rings[0].radius;
        for _ in 0..24 {
            world.step(0.0, false, false, false);
        }
        assert!(world.rings[0].radius > r0);
        assert!(world.rings[0].life < 1.0 && world.rings[0].life > 0.0);
        for _ in 0..200 {
            world.step(0.0, false, false, false);
        }
        assert!(world.rings.is_empty());
    }

    #[test]
    fn impact_scales_squash_and_glow() {
        let mut heavy = flat();
        heavy.balls[0].y = 10.0 + RADIUS + 20.0;
        heavy.balls[0].vy = 0.0;
        for _ in 0..400 {
            heavy.step(0.0, false, false, false);
            if heavy.balls[0].squash > 0.0 {
                break;
            }
        }
        let mut light = flat();
        light.balls[0].y = 10.0 + RADIUS + 0.4;
        light.balls[0].vy = 0.0;
        for _ in 0..400 {
            light.step(0.0, false, false, false);
            if light.balls[0].squash > 0.0 {
                break;
            }
        }
        assert!(heavy.balls[0].squash > light.balls[0].squash);
        assert!(heavy.balls[0].impact_glow > light.balls[0].impact_glow);
        assert!(heavy.balls[0].impact_glow <= 1.0);
    }

    #[test]
    fn impact_glow_decays() {
        let mut world = flat();
        world.balls[0].impact_glow = 1.0;
        world.balls[0].y = 30.0;
        world.balls[0].vy = 0.0;
        for _ in 0..40 {
            world.step(0.0, false, false, false);
        }
        let glow = world.balls[0].impact_glow;
        assert!(
            glow < 1.0 && glow > 0.0,
            "glow must decay over ~0.3s: {}",
            glow
        );
    }

    #[test]
    fn reset_and_resize_clear_transient_state() {
        let mut world = flat();
        world.balls[0].y = 10.0 + RADIUS;
        world.jump();
        assert_eq!(world.rings.len(), 1);
        world.reset(3);
        assert!(world.rings.is_empty());
        assert_eq!(world.balls.len(), 3);
        world.jump();
        assert_eq!(world.rings.len(), 3);
        world.resize(80.0, 40.0);
        assert!(world.rings.is_empty());
        for _ in 0..60 {
            world.step(0.0, false, false, false);
        }
        for b in &world.balls {
            assert!(b.x.is_finite() && b.y.is_finite());
            assert!(b.y <= world.height - RADIUS);
        }
    }

    #[test]
    fn jagged_terrain_clears_the_entire_circle_footprint() {
        let mut world = World::new(80.0, 48.0, 38.0, 3);
        let bands =
            std::array::from_fn::<_, BANDS, _>(|i| if (i / 3) % 2 == 0 { 0.85 } else { 0.0 });
        world.set_surface(&bands);
        for _ in 0..7200 {
            world.step(0.0, false, false, false);
            for b in &world.balls {
                for j in 0..=32 {
                    let dx = (j as f32 / 16.0 - 1.0) * RADIUS;
                    let bottom = b.y - (RADIUS * RADIUS - dx * dx).max(0.0).sqrt();
                    let surface = World::height_at(&world.surface, world.width, b.x + dx);
                    assert!(bottom + 0.001 >= surface, "circle intersects terrain");
                }
            }
        }
    }

    #[test]
    fn quiet_bounce_height_survives_high_gravity() {
        for gravity in [5.0, 38.0, 150.0] {
            let mut world = World::new(100.0, 80.0, gravity, 1);
            let start = 1.0 + RADIUS;
            world.balls[0].y = start;
            world.balls[0].vy = -1.0;
            let mut peak = start;
            for _ in 0..1920 {
                world.step(0.0, false, false, false);
                peak = peak.max(world.balls[0].y);
            }
            assert!(peak - start > (world.height - RADIUS - start) * 0.12);
        }
    }

    #[test]
    fn steep_repeated_valleys_do_not_confine_balls() {
        let mut world = World::new(80.0, 48.0, 38.0, 3);
        let bands =
            std::array::from_fn::<_, BANDS, _>(|i| if (i / 3) % 2 == 0 { 0.85 } else { 0.0 });
        world.set_surface(&bands);
        let mut left = [f32::INFINITY; 3];
        let mut right = [f32::NEG_INFINITY; 3];
        for tick in 0..10800 {
            world.step(0.0, false, false, false);
            if tick > 5400 {
                for (i, b) in world.balls.iter().enumerate() {
                    left[i] = left[i].min(b.x);
                    right[i] = right[i].max(b.x);
                }
            }
        }
        for i in 0..3 {
            assert!(right[i] - left[i] > 10.0, "ball {i} remained trapped");
        }
    }

    #[test]
    fn airborne_onset_responds_to_each_balls_local_bands() {
        let mut world = World::new(100.0, 50.0, 38.0, 2);
        world.target[BANDS / 2..].fill(25.0);
        for b in &mut world.balls {
            b.personality = Personality::Balanced;
            b.y = 30.0;
            b.vy = 0.0;
        }
        world.step(0.0, true, false, false);
        assert!(world.balls[1].vy > world.balls[0].vy + 2.0);
        assert!(world.balls[0].vy > 10.0);
    }
}
