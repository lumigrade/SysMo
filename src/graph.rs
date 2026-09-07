/* graph.rs
 *
 * A faithful port of GNOME System Monitor's load graph (load-graph.cpp and
 * gsm-graph.c, GPL-2.0-or-later, GNOME System Monitor developers) to a
 * GTK4 DrawingArea drawn with cairo.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use std::cell::RefCell;
use std::rc::Rc;

use gtk::{cairo, gdk, pango, prelude::*};

use crate::units;

/// Redraw interval and frames per sample: System Monitor redraws every 100 ms
/// and takes a new sample every ten frames, i.e. once per second.
pub const UPDATE_INTERVAL_MS: u64 = 100;
pub const FRAMES_PER_UNIT: u32 = 10;
/// Visible samples per graph; 60 samples at one per second span one minute.
pub const DATA_POINTS: usize = 60;

const FRAME_WIDTH: f64 = 4.0;
const BORDER_ALPHA: f64 = 0.7;
const GRID_ALPHA: f64 = BORDER_ALPHA / 2.0;
const NUM_SECTIONS: usize = 7;
const INDENT: f64 = 18.0;
const MIN_HEIGHT: i32 = 70;
const NO_DATA: f64 = -1.0;
const BASE_FONT_SIZE: f64 = 8.0;
/// The plot pane is drawn translucent so the desktop shows through the
/// "glass" window while the curves stay readable.
const PLOT_ALPHA: f64 = 0.35;
const DASH_PATTERN: [f64; 2] = [5.0, 3.0];

/// How one series is drawn.
#[derive(Clone, Copy, Debug)]
pub struct Series {
    pub color: gdk::RGBA,
    pub dashed: bool,
    pub width: f64,
}

impl Series {
    pub fn solid(color: gdk::RGBA) -> Self {
        Self {
            color,
            dashed: false,
            width: 1.0,
        }
    }

    pub fn dashed(color: gdk::RGBA) -> Self {
        Self {
            color,
            dashed: true,
            width: 1.5,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scale {
    /// Fixed 0-100 % axis.
    Percent,
    /// Auto-scaled bytes-per-second axis (network and disk graphs).
    Rate,
}

struct CachedBackground {
    surface: cairo::ImageSurface,
    width: i32,
    height: i32,
    scale_factor: i32,
}

struct State {
    scale: Scale,
    num_points: usize,
    series: usize,
    styles: Vec<Series>,
    /// `data[i][j]`: sample `i` (0 = newest) of series `j`, as a fraction of
    /// the axis maximum; `NO_DATA` where nothing has been recorded yet.
    data: Vec<Vec<f64>>,
    render_counter: u32,
    smooth: bool,
    font_size: f64,
    foreground: gdk::RGBA,
    plot_background: gdk::RGBA,
    /// Current axis maximum for `Scale::Rate`, in bytes per second.
    max: u64,
    /// Peak of each visible sample, used to shrink the axis again.
    peaks: Vec<u64>,
    background: Option<CachedBackground>,
}

impl State {
    fn reset_data(&mut self) {
        self.data = vec![vec![NO_DATA; self.series]; self.num_points];
        self.peaks = vec![0; self.num_points];
    }

    fn rotate(&mut self) {
        self.data.rotate_right(1);
        for value in &mut self.data[0] {
            *value = NO_DATA;
        }
    }

    fn caption(&self, index: u32, num_bars: u32) -> String {
        match self.scale {
            Scale::Percent => {
                let percent = 100.0 - index as f64 * 100.0 / num_bars as f64;
                format!("{percent:.0} %")
            }
            Scale::Rate => {
                let max = self.max as f64;
                units::rate((max - index as f64 * max / num_bars as f64) as u64)
            }
        }
    }
}

/// One time-series graph. Cheap to clone; clones share the same widget.
#[derive(Clone)]
pub struct LoadGraph {
    area: gtk::DrawingArea,
    state: Rc<RefCell<State>>,
}

impl LoadGraph {
    pub fn new(scale: Scale, styles: Vec<Series>, text_scaling: f64) -> Self {
        let area = gtk::DrawingArea::new();
        area.set_size_request(-1, MIN_HEIGHT);
        area.set_hexpand(true);
        area.set_vexpand(true);

        let series = styles.len();
        let mut state = State {
            scale,
            num_points: DATA_POINTS + 2,
            series,
            styles,
            data: Vec::new(),
            render_counter: 0,
            smooth: true,
            font_size: BASE_FONT_SIZE * text_scaling,
            foreground: gdk::RGBA::new(1.0, 1.0, 1.0, 1.0),
            plot_background: gdk::RGBA::new(0.0, 0.0, 0.0, PLOT_ALPHA as f32),
            max: 1024,
            peaks: Vec::new(),
            background: None,
        };
        state.reset_data();
        let state = Rc::new(RefCell::new(state));

        area.set_draw_func({
            let state = state.clone();
            move |area, cr, width, height| {
                let mut state = state.borrow_mut();
                if let Err(e) = draw(&mut state, area, cr, width, height) {
                    gtk::glib::g_warning!("SysMo::Graph", "Drawing failed: {}", e);
                }
            }
        });

        area.connect_resize({
            let state = state.clone();
            move |_, _, _| state.borrow_mut().background = None
        });
        area.connect_scale_factor_notify({
            let state = state.clone();
            move |_| state.borrow_mut().background = None
        });

        Self { area, state }
    }

    pub fn widget(&self) -> &gtk::DrawingArea {
        &self.area
    }

    /// Replace the series set (used once the CPU core count is known).
    pub fn set_series(&self, styles: Vec<Series>) {
        let mut state = self.state.borrow_mut();
        state.series = styles.len();
        state.styles = styles;
        state.reset_data();
        drop(state);
        self.area.queue_draw();
    }

    /// Record one sample for a `Scale::Percent` graph. Values are fractions
    /// in `0.0..=1.0`; anything negative means "no data" for that series.
    pub fn push_fractions(&self, values: &[f64]) {
        let mut state = self.state.borrow_mut();
        state.rotate();
        for (j, value) in values.iter().enumerate().take(state.series) {
            state.data[0][j] = if *value < 0.0 {
                NO_DATA
            } else {
                value.clamp(0.0, 1.0)
            };
        }
        state.render_counter = 0;
        drop(state);
        self.area.queue_draw();
    }

    /// Record one sample for a `Scale::Rate` graph, rescaling the axis the
    /// way System Monitor does.
    pub fn push_rates(&self, incoming: u64, outgoing: u64) {
        let mut state = self.state.borrow_mut();
        state.rotate();

        let peak = incoming.max(outgoing);
        state.peaks.rotate_right(1);
        state.peaks[0] = peak;

        let raw_max = if peak >= state.max {
            peak
        } else {
            state.peaks.iter().copied().max().unwrap_or(peak)
        };
        let new_max = round_axis_max(raw_max);
        let old_max = state.max;

        // Keep the current axis if the new maximum is the same or only a
        // little smaller, to avoid rescaling on every sample.
        let keep = (0.8 * old_max as f64) < new_max as f64 && new_max <= old_max;
        if !keep && new_max != old_max {
            let factor = old_max as f64 / new_max as f64;
            for row in state.data.iter_mut() {
                if row[0] >= 0.0 {
                    row[0] *= factor;
                    row[1] *= factor;
                }
            }
            state.max = new_max;
            state.background = None;
        }

        let max = state.max as f64;
        state.data[0][0] = incoming as f64 / max;
        state.data[0][1] = outgoing as f64 / max;
        state.render_counter = 0;
        drop(state);
        self.area.queue_draw();
    }

    /// Advance the smooth-scroll animation by one frame.
    pub fn tick(&self) {
        let mut state = self.state.borrow_mut();
        if state.render_counter < FRAMES_PER_UNIT - 1 {
            state.render_counter += 1;
        }
        drop(state);
        self.area.queue_draw();
    }
}

/// System Monitor's axis rounding: 10 % headroom, at least 1 KiB/s, and a
/// single significant digit in units of 1024^n.
fn round_axis_max(value: u64) -> u64 {
    let new_max = ((1.1 * value as f64) as u64).max(1024);
    let pow2 = (new_max as f64).log2().floor() as u64;
    let base10 = pow2 / 10;
    let unit = 1u64 << (base10 * 10);
    let mut coef10 = (new_max as f64 / unit as f64).ceil().max(1.0) as u64;
    let factor10 = 10f64.powf((coef10 as f64).log10().floor()) as u64;
    coef10 = ((coef10 as f64 / factor10 as f64).ceil() as u64) * factor10;
    coef10 * unit
}

fn num_bars(height: f64, font_size: f64) -> u32 {
    match (height / (font_size + 14.0)) as i32 {
        i32::MIN..=1 => 1,
        2 | 3 => 2,
        4 => 4,
        _ => 5,
    }
}

fn set_source(cr: &cairo::Context, color: &gdk::RGBA, alpha: f64) {
    cr.set_source_rgba(
        color.red() as f64,
        color.green() as f64,
        color.blue() as f64,
        alpha,
    );
}

struct Geometry {
    width: f64,
    height: f64,
    rmargin: f64,
    num_bars: u32,
    graph_dely: f64,
    real_draw_height: f64,
}

fn draw(
    state: &mut State,
    area: &gtk::DrawingArea,
    cr: &cairo::Context,
    alloc_width: i32,
    alloc_height: i32,
) -> Result<(), cairo::Error> {
    let width = alloc_width as f64 - 2.0 * FRAME_WIDTH;
    let height = alloc_height as f64 - 2.0 * FRAME_WIDTH;
    let font_size = state.font_size;
    let num_bars = num_bars(height, font_size);
    let graph_dely = ((height - 15.0) / num_bars as f64).floor().max(1.0);
    let geometry = Geometry {
        width,
        height,
        rmargin: 6.0 * font_size,
        num_bars,
        graph_dely,
        real_draw_height: graph_dely * num_bars as f64,
    };

    let x_step = (width - geometry.rmargin - INDENT) / (state.num_points as f64 - 2.0);
    let mut x_offset = width - geometry.rmargin + FRAME_WIDTH;
    x_offset += x_step * (1.0 - state.render_counter as f64 / FRAMES_PER_UNIT as f64);

    let scale_factor = area.scale_factor();
    let stale = match &state.background {
        Some(bg) => {
            bg.width != alloc_width || bg.height != alloc_height || bg.scale_factor != scale_factor
        }
        None => true,
    };
    if stale {
        state.background = Some(create_background(
            state,
            area,
            &geometry,
            alloc_width,
            alloc_height,
            scale_factor,
        )?);
    }
    if let Some(bg) = &state.background {
        cr.set_source_surface(&bg.surface, 0.0, 0.0)?;
        cr.paint()?;
    }

    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);

    cr.rectangle(
        INDENT + FRAME_WIDTH,
        FRAME_WIDTH,
        width - geometry.rmargin - INDENT,
        geometry.real_draw_height,
    );
    cr.clip();

    let y_base = FRAME_WIDTH;
    let h = geometry.real_draw_height;

    for j in (0..state.series).rev() {
        let style = state.styles[j];
        set_source(cr, &style.color, style.color.alpha() as f64);
        cr.set_line_width(style.width);
        if style.dashed {
            cr.set_dash(&DASH_PATTERN, 0.0);
        } else {
            cr.set_dash(&[], 0.0);
        }

        cr.move_to(x_offset, y_base + (1.0 - state.data[0][j]) * h);
        for i in 1..state.num_points {
            let value = state.data[i][j];
            if value == NO_DATA {
                continue;
            }
            let x = x_offset - i as f64 * x_step;
            let y = y_base + (1.0 - value) * h;
            if state.smooth {
                let previous = state.data[i - 1][j];
                let cx = x_offset - (i as f64 - 0.5) * x_step;
                cr.curve_to(cx, y_base + (1.0 - previous) * h, cx, y, x, y);
            } else {
                cr.line_to(x, y);
            }
        }
        cr.stroke()?;
    }

    Ok(())
}

fn create_background(
    state: &State,
    area: &gtk::DrawingArea,
    g: &Geometry,
    alloc_width: i32,
    alloc_height: i32,
    scale_factor: i32,
) -> Result<CachedBackground, cairo::Error> {
    let surface = cairo::ImageSurface::create(
        cairo::Format::ARgb32,
        alloc_width * scale_factor,
        alloc_height * scale_factor,
    )?;
    surface.set_device_scale(scale_factor as f64, scale_factor as f64);
    let cr = cairo::Context::new(&surface)?;

    let layout = pangocairo::functions::create_layout(&cr);
    let mut font = area
        .pango_context()
        .font_description()
        .unwrap_or_default();
    font.set_size((0.8 * state.font_size * pango::SCALE as f64) as i32);
    layout.set_font_description(Some(&font));

    cr.translate(FRAME_WIDTH, FRAME_WIDTH);

    // Plot background.
    set_source(&cr, &state.plot_background, state.plot_background.alpha() as f64);
    cr.rectangle(INDENT, 0.0, g.width - g.rmargin - INDENT, g.real_draw_height);
    cr.fill()?;

    cr.set_line_width(0.25);
    let fg = state.foreground;
    let pango_scale = pango::SCALE as f64;

    // Horizontal grid lines and value captions.
    for i in 0..=g.num_bars {
        let y = if i == 0 {
            0.5 + state.font_size / 2.0
        } else if i == g.num_bars {
            i as f64 * g.graph_dely + 0.5
        } else {
            i as f64 * g.graph_dely + state.font_size / 2.0
        };

        layout.set_text(&state.caption(i, g.num_bars));
        layout.set_alignment(pango::Alignment::Left);
        let (_, extents) = layout.extents();
        let modifier = if i == 0 {
            0.5
        } else if i == g.num_bars {
            1.0
        } else {
            0.85
        };

        cr.move_to(
            g.width - INDENT - 23.0,
            y - modifier * extents.height() as f64 / pango_scale,
        );
        set_source(&cr, &fg, fg.alpha() as f64);
        pangocairo::functions::show_layout(&cr, &layout);

        let alpha = if i == 0 || i == g.num_bars {
            BORDER_ALPHA
        } else {
            GRID_ALPHA
        };
        set_source(&cr, &fg, alpha);
        cr.move_to(INDENT, i as f64 * g.graph_dely);
        cr.line_to(g.width - g.rmargin + 4.0, i as f64 * g.graph_dely);
        cr.stroke()?;
    }

    // Vertical grid lines and time captions.
    let total_seconds =
        (UPDATE_INTERVAL_MS as u64 * (state.num_points as u64 - 2) / 1000 * FRAMES_PER_UNIT as u64)
            as u32;
    for i in 0..NUM_SECTIONS {
        let x = (i as f64 * (g.width - g.rmargin - INDENT) / (NUM_SECTIONS as f64 - 1.0)).ceil();

        let seconds = total_seconds - i as u32 * total_seconds / (NUM_SECTIONS as u32 - 1);
        layout.set_text(&units::duration(seconds));
        let (_, extents) = layout.extents();
        let modifier = if i == 0 {
            0.0
        } else if i == NUM_SECTIONS - 1 {
            1.0
        } else {
            0.5
        };

        cr.move_to(
            x + INDENT - modifier * extents.width() as f64 / pango_scale + 1.0,
            g.height - extents.height() as f64 / pango_scale,
        );
        set_source(&cr, &fg, fg.alpha() as f64);
        pangocairo::functions::show_layout(&cr, &layout);

        let alpha = if i == 0 || i == NUM_SECTIONS - 1 {
            BORDER_ALPHA
        } else {
            GRID_ALPHA
        };
        set_source(&cr, &fg, alpha);
        cr.move_to(x + INDENT, 0.0);
        cr.line_to(x + INDENT, g.real_draw_height + 4.0);
        cr.stroke()?;
    }

    Ok(CachedBackground {
        surface,
        width: alloc_width,
        height: alloc_height,
        scale_factor,
    })
}
