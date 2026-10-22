mod audio;
mod capture;
mod cli;
mod input;
mod physics;
mod render;
mod spectrum;

use anyhow::{bail, Context, Result};
use clap::Parser;
use crossterm::{event, terminal};
use input::Action;
use std::{
    io::{self, IsTerminal},
    time::{Duration, Instant},
};

fn main() -> Result<()> {
    let args = cli::Args::parse();
    if args.list_sources {
        return capture::list_sources();
    }
    if !io::stdout().is_terminal() {
        bail!("visualization requires a terminal on stdout; use --help or --list-sources for plain output");
    }
    let mut audio = if args.demo {
        audio::Audio::demo(args.sample_rate)
    } else if args.stdin {
        capture::stdin(args.sample_rate)?
    } else {
        capture::open(args.source.as_deref(), args.microphone, args.sample_rate)?
    };
    let mut spectrum = spectrum::Spectrum::new(args.sample_rate, args.sensitivity);
    let _terminal = render::Terminal::enter().context("could not initialize terminal")?;
    let (mut cols, mut rows) = terminal::size()?;
    let mut world = physics::World::new(cols as f32, field_height(rows), args.gravity, args.balls);
    let mut renderer = render::Renderer::new(args.color);
    let mut view = render::View {
        palette: args.palette,
        sparks: true,
        stars: true,
        trails: args.trails,
        help: false,
        paused: false,
        time: 0.0,
    };
    let mut input = input::Input::new();
    let mut samples = [0.0; spectrum::FFT_SIZE];
    let mut status = String::with_capacity(256);
    let mut last = Instant::now();
    let mut accumulator = 0.0;
    let mut pending_beat = false;
    let frame = Duration::from_secs_f64(1.0 / args.fps as f64);
    let mut next_frame = last;
    'running: loop {
        let remaining = next_frame.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            event::poll(remaining)?;
        }
        let input_at = Instant::now();
        for _ in 0..128 {
            // A positive timeout lets the /dev/tty backend service its OS poll
            // even when rendering has already consumed the frame deadline.
            if !event::poll(Duration::from_millis(1))? {
                break;
            }
            if let Some(action) = input.handle(event::read()?, input_at) {
                match action {
                    Action::Quit => break 'running,
                    Action::Jump => {
                        if !view.paused {
                            world.jump();
                        }
                    }
                    Action::AddBall => world.add_ball(),
                    Action::RemoveBall => world.remove_ball(),
                    Action::Pause => {
                        view.paused = !view.paused;
                        accumulator = 0.0;
                        pending_beat = false;
                    }
                    Action::Palette => view.palette = view.palette.next(),
                    Action::Sparks => view.sparks = !view.sparks,
                    Action::Stars => view.stars = !view.stars,
                    Action::Trails => view.trails = !view.trails,
                    Action::Help => view.help = !view.help,
                    Action::Reset => world.reset(world.balls.len()),
                    Action::Resize(c, r) => {
                        cols = c;
                        rows = r;
                        world.resize(cols as f32, field_height(rows));
                        accumulator = 0.0;
                    }
                }
            }
        }
        let start = Instant::now();
        if start < next_frame {
            // Input can wake the wait early, but must not increase render FPS.
            continue;
        }
        // Discard long scheduler stalls rather than running an unbounded catch-up loop.
        let dt = (start - last).as_secs_f32().min(0.1);
        last = start;
        // Capture continuously drains on its worker, including while paused or undersized.
        let (changed, silent) = audio.snapshot(&mut samples, &mut status);
        let beat = spectrum.update(changed.then_some(samples.as_slice()), silent, dt);
        if !view.paused && cols >= 24 && rows >= 10 {
            world.set_surface(&spectrum.heights);
            view.time += dt;
            pending_beat |= beat;
            accumulator += dt;
            while accumulator >= physics::STEP {
                world.step(input.steer(start), pending_beat, view.sparks, view.trails);
                pending_beat = false;
                accumulator -= physics::STEP;
            }
        } else {
            accumulator = 0.0;
            pending_beat = false;
        }
        renderer.draw(&world, cols, rows, &status, &view)?;
        next_frame += frame;
        let finished = Instant::now();
        if next_frame <= finished {
            next_frame = finished + frame;
        }
    }
    Ok(())
}

fn field_height(rows: u16) -> f32 {
    rows.saturating_sub(4).max(6) as f32 * physics::CELL_ASPECT
}
