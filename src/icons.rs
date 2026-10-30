/* icons.rs - the small CPU and GPU badges of the Network tab, drawn with
 * cairo so they stay sharp at any size: a processor chip on blue and a
 * two-fan graphics card on yellow.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use std::f64::consts::PI;

use gtk::cairo::{Context, LineCap};
use gtk::prelude::*;

type Rgb = (f64, f64, f64);

fn rgb(hex: u32) -> Rgb {
    let channel = |shift: u32| ((hex >> shift) & 0xff) as f64 / 255.0;
    (channel(16), channel(8), channel(0))
}

fn rounded_rect(cr: &Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 1.5 * PI);
    cr.close_path();
}

/// A processor: a rounded chip with three pins a side and a solid core, on
/// a 16-unit grid.
fn draw_cpu(cr: &Context) {
    cr.set_line_cap(LineCap::Round);
    cr.set_line_width(1.4);
    rounded_rect(cr, 3.6, 3.6, 8.8, 8.8, 1.8);
    let _ = cr.stroke();
    rounded_rect(cr, 6.2, 6.2, 3.6, 3.6, 0.8);
    let _ = cr.fill();
    cr.set_line_width(1.2);
    for p in [5.6, 8.0, 10.4] {
        for (x1, y1, x2, y2) in [
            (p, 1.2, p, 3.0),
            (p, 13.0, p, 14.8),
            (1.2, p, 3.0, p),
            (13.0, p, 14.8, p),
        ] {
            cr.move_to(x1, y1);
            cr.line_to(x2, y2);
        }
    }
    let _ = cr.stroke();
}

/// A graphics card: a rounded board with two fans and its edge connector,
/// on a 16-unit grid.
fn draw_gpu(cr: &Context) {
    cr.set_line_cap(LineCap::Round);
    cr.set_line_width(1.4);
    rounded_rect(cr, 0.9, 3.2, 14.2, 8.4, 1.8);
    let _ = cr.stroke();
    cr.set_line_width(1.2);
    for x in [5.1, 10.9] {
        cr.new_sub_path();
        cr.arc(x, 7.4, 2.2, 0.0, 2.0 * PI);
        let _ = cr.stroke();
        cr.arc(x, 7.4, 0.7, 0.0, 2.0 * PI);
        let _ = cr.fill();
    }
    cr.set_line_width(1.6);
    cr.move_to(4.0, 13.6);
    cr.line_to(9.5, 13.6);
    let _ = cr.stroke();
}

/// A `size` px badge: the glyph on a filled rounded square.
fn badge(size: i32, fill: Rgb, glyph: Rgb, draw: fn(&Context)) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_content_width(size);
    area.set_content_height(size);
    area.set_valign(gtk::Align::Center);
    area.set_draw_func(move |_, cr, width, _| {
        let s = width as f64;
        cr.set_source_rgb(fill.0, fill.1, fill.2);
        rounded_rect(cr, 0.0, 0.0, s, s, s * 0.22);
        let _ = cr.fill();
        cr.translate(s * 0.14, s * 0.14);
        cr.scale(s * 0.72 / 16.0, s * 0.72 / 16.0);
        cr.set_source_rgb(glyph.0, glyph.1, glyph.2);
        draw(cr);
    });
    area
}

/// White processor chip on blue.
pub fn cpu_badge(size: i32) -> gtk::DrawingArea {
    badge(size, rgb(0x3584e4), rgb(0xffffff), draw_cpu)
}

/// Dark graphics card on yellow.
pub fn gpu_badge(size: i32) -> gtk::DrawingArea {
    badge(size, rgb(0xf6d32d), rgb(0x241f31), draw_gpu)
}
