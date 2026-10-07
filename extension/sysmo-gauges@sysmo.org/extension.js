/* SysMo Gauges - GNOME Shell extension
 *
 * Five ring gauges in the top bar: CPU load, CPU temperature, GPU load,
 * VRAM use and GPU temperature. The figures come from the SysMo app over
 * D-Bus (org.sysmo.SysMo, /org/sysmo/SysMo/Gauges), so SysMo has to be
 * running; clicking a gauge starts or raises it.
 *
 * It also keeps the SysMo window on top while the app's pin button is on.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Shell from 'gi://Shell';
import St from 'gi://St';
import Cairo from 'cairo';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';

const BUS_NAME = 'org.sysmo.SysMo';
const OBJECT_PATH = '/org/sysmo/SysMo/Gauges';
const INTERFACE = 'org.sysmo.SysMo.Gauges';
const APP_DESKTOP_ID = 'org.sysmo.SysMo.desktop';
const SETTINGS_SCHEMA = 'org.sysmo.SysMo';
const STAY_ON_TOP_KEY = 'stay-on-top';

const GAUGE_SIZE = 28;
const RING_WIDTH = 2.5;
const HOVER_SCALE = 1.15;
const ARC_SEGMENTS_PER_TURN = 96;

// Identity colours (track ring and hover glow) match the curves in the
// SysMo window.
const GAUGES = [
    {key: 'cpu-load', title: 'CPU load', unit: '%', device: 'cpu', color: [0.878, 0.106, 0.141]},
    {key: 'cpu-temp', title: 'CPU temperature', unit: '°C', device: 'cpu', color: [1.000, 0.471, 0.000]},
    {key: 'gpu-load', title: 'GPU load', unit: '%', device: 'gpu', color: [0.000, 0.902, 0.463]},
    {key: 'gpu-vram', title: 'GPU memory (VRAM)', unit: '%', device: 'gpu', color: [0.965, 0.827, 0.176]},
    {key: 'gpu-temp', title: 'GPU temperature', unit: '°C', device: 'gpu', color: [0.208, 0.518, 0.894]},
];

// Colour of the arc at a given reading: cool blue at rest, then the same
// green / yellow / red the background uses from 45 / 65 / 80.
const HEAT_STOPS = [
    [0, [0.208, 0.518, 0.894]],
    [45, [0.200, 0.820, 0.478]],
    [65, [0.965, 0.827, 0.176]],
    [80, [0.878, 0.106, 0.141]],
    [100, [0.700, 0.080, 0.120]],
];

function heatColor(value) {
    const v = Math.max(0, Math.min(100, value));
    for (let i = 1; i < HEAT_STOPS.length; i++) {
        const [v0, c0] = HEAT_STOPS[i - 1];
        const [v1, c1] = HEAT_STOPS[i];
        if (v <= v1) {
            const t = (v - v0) / (v1 - v0);
            return c0.map((c, k) => c + (c1[k] - c) * t);
        }
    }
    return HEAT_STOPS[HEAT_STOPS.length - 1][1];
}

// Background disc behind the number. It fades in as an amber-yellow from
// 55, is fully yellow at 65, blends through orange to red by 80 and deepens
// towards 100. No jumps, and no green: yellow and red are the warnings.
const DISC_YELLOW = [0.929, 0.702, 0.102];
const DISC_RED = [0.878, 0.106, 0.141];
const DISC_DEEP_RED = [0.700, 0.080, 0.120];
const DISC_ALPHA = 0.84;
const SHADOW = 'text-shadow: 0px 1px 2px rgba(0, 0, 0, 0.9);';

function lerpColor(a, b, t) {
    return a.map((c, i) => c + (b[i] - c) * t);
}

function stateFor(value) {
    if (value === null)
        return {background: null, alpha: 0, text: 'rgba(255, 255, 255, 0.45)', shadow: ''};
    let background = null;
    let alpha = 0;
    if (value >= 80) {
        background = lerpColor(DISC_RED, DISC_DEEP_RED, Math.min(1, (value - 80) / 20));
        alpha = DISC_ALPHA;
    } else if (value >= 65) {
        background = lerpColor(DISC_YELLOW, DISC_RED, (value - 65) / 15);
        alpha = DISC_ALPHA;
    } else if (value >= 55) {
        background = DISC_YELLOW;
        alpha = DISC_ALPHA * (value - 55) / 10;
    }
    return {background, alpha, text: 'white', shadow: SHADOW};
}

const Gauge = GObject.registerClass(
class SysmoGauge extends St.Button {
    _init(spec) {
        super._init({
            style_class: 'sysmo-gauge',
            reactive: true,
            can_focus: false,
            track_hover: true,
            y_align: Clutter.ActorAlign.CENTER,
        });
        this.spec = spec;
        this._value = null;
        this._hovered = false;
        this.set_pivot_point(0.5, 0.5);

        const stack = new St.Widget({
            layout_manager: new Clutter.BinLayout(),
            width: GAUGE_SIZE,
            height: GAUGE_SIZE,
        });
        this._area = new St.DrawingArea({width: GAUGE_SIZE, height: GAUGE_SIZE});
        this._area.connect('repaint', () => this._repaint());
        this._label = new St.Label({
            style_class: 'sysmo-gauge-number',
            text: '–',
            x_align: Clutter.ActorAlign.CENTER,
            y_align: Clutter.ActorAlign.CENTER,
            x_expand: true,
            y_expand: true,
        });
        stack.add_child(this._area);
        stack.add_child(this._label);
        this.set_child(stack);

        this.connect('notify::hover', () => this._onHoverChanged());
    }

    get value() {
        return this._value;
    }

    setValue(value) {
        this._value = Number.isFinite(value) ? value : null;
        const state = stateFor(this._value);
        this._label.text = this._value === null ? '–' : String(Math.round(this._value));
        this._label.set_style(`color: ${state.text}; ${state.shadow}`);
        this._area.queue_repaint();
    }

    _onHoverChanged() {
        this._hovered = this.hover;
        const scale = this._hovered ? HOVER_SCALE : 1.0;
        this.ease({
            scale_x: scale,
            scale_y: scale,
            duration: 150,
            mode: Clutter.AnimationMode.EASE_OUT_QUAD,
        });
        this._area.queue_repaint();
    }

    _repaint() {
        const cr = this._area.get_context();
        const [width, height] = this._area.get_surface_size();
        const cx = width / 2;
        const cy = height / 2;
        const radius = Math.min(width, height) / 2 - RING_WIDTH / 2 - 1;
        const state = stateFor(this._value);
        const [ir, ig, ib] = this.spec.color;

        // See-through glossy disc for the warning states.
        if (state.background) {
            const [br, bg, bb] = state.background;
            const inner = radius - RING_WIDTH / 2;
            cr.arc(cx, cy, inner, 0, 2 * Math.PI);
            cr.setSourceRGBA(br, bg, bb, state.alpha);
            cr.fill();

            const gloss = new Cairo.LinearGradient(0, cy - inner, 0, cy - inner * 0.1);
            gloss.addColorStopRGBA(0, 1, 1, 1, 0.38);
            gloss.addColorStopRGBA(1, 1, 1, 1, 0.0);
            cr.save();
            cr.arc(cx, cy, inner, 0, 2 * Math.PI);
            cr.clip();
            cr.setSource(gloss);
            cr.paint();
            cr.restore();
        }

        // Soft glow in the gauge's identity colour while hovered.
        if (this._hovered) {
            cr.setLineWidth(RING_WIDTH + 4);
            cr.setSourceRGBA(ir, ig, ib, 0.30);
            cr.arc(cx, cy, radius, 0, 2 * Math.PI);
            cr.stroke();
        }

        // Track, tinted with the identity colour.
        cr.setLineCap(Cairo.LineCap.BUTT);
        cr.setLineWidth(this._hovered ? RING_WIDTH + 0.75 : RING_WIDTH);
        cr.setSourceRGBA(ir, ig, ib, this._value === null ? 0.18 : 0.34);
        cr.arc(cx, cy, radius, 0, 2 * Math.PI);
        cr.stroke();

        // Value arc from 12 o'clock, clockwise, a full turn is 100. Drawn as
        // short segments so the colour can follow the reading along the arc.
        if (this._value !== null) {
            const fraction = Math.max(0, Math.min(1, this._value / 100));
            if (fraction > 0) {
                const start = -Math.PI / 2;
                const sweep = fraction * 2 * Math.PI;
                const segments = Math.max(1, Math.ceil(fraction * ARC_SEGMENTS_PER_TURN));
                const step = sweep / segments;
                const overlap = Math.min(step * 0.5, 0.02);
                for (let i = 0; i < segments; i++) {
                    const a0 = start + i * step;
                    const a1 = a0 + step + (i < segments - 1 ? overlap : 0);
                    const [r, g, b] = heatColor(((i + 0.5) / segments) * fraction * 100);
                    cr.setSourceRGBA(r, g, b, this._hovered ? 1.0 : 0.95);
                    cr.arc(cx, cy, radius, a0, a1);
                    cr.stroke();
                }

                // Round ends: a dot at the start and at the tip.
                const capRadius = cr.getLineWidth() / 2;
                const [sr, sg, sb] = heatColor(0);
                cr.setSourceRGBA(sr, sg, sb, 0.95);
                cr.arc(cx + radius * Math.cos(start), cy + radius * Math.sin(start), capRadius, 0, 2 * Math.PI);
                cr.fill();
                const tip = start + sweep;
                const [tr, tg, tb] = heatColor(fraction * 100);
                cr.setSourceRGBA(tr, tg, tb, 1.0);
                cr.arc(cx + radius * Math.cos(tip), cy + radius * Math.sin(tip), capRadius, 0, 2 * Math.PI);
                cr.fill();
            }
        }

        cr.$dispose();
    }
});

const Indicator = GObject.registerClass(
class SysmoGaugesIndicator extends PanelMenu.Button {
    _init() {
        super._init(0.0, 'SysMo Gauges', true);
        // Only the hovered gauge should react, not the whole group.
        this.add_style_class_name('sysmo-gauges-indicator');

        this._names = {};
        this._details = {};
        this._alive = false;
        this._proxy = null;
        this._hoveredGauge = null;

        this._box = new St.BoxLayout({
            style_class: 'sysmo-gauges',
            y_align: Clutter.ActorAlign.CENTER,
        });
        this.add_child(this._box);

        this._gauges = new Map();
        for (const spec of GAUGES) {
            const gauge = new Gauge(spec);
            gauge.connect('clicked', () => this._activateApp());
            gauge.connect('notify::hover', () => this._onGaugeHover(gauge));
            this._gauges.set(spec.key, gauge);
            this._box.add_child(gauge);
        }

        this._tooltip = new St.Label({style_class: 'sysmo-gauge-tooltip', visible: false});
        this._tooltip.clutter_text.use_markup = true;
        Main.layoutManager.uiGroup.add_child(this._tooltip);

        this.connect('destroy', () => this._onDestroy());
        this._connectProxy();
    }

    _connectProxy() {
        Gio.DBusProxy.new_for_bus(
            Gio.BusType.SESSION,
            Gio.DBusProxyFlags.DO_NOT_AUTO_START,
            null,
            BUS_NAME,
            OBJECT_PATH,
            INTERFACE,
            null,
            (_source, result) => {
                try {
                    this._proxy = Gio.DBusProxy.new_for_bus_finish(result);
                } catch (e) {
                    console.warn(`SysMo Gauges: cannot create D-Bus proxy: ${e.message}`);
                    return;
                }
                this._proxy.connect('g-properties-changed', () => this._refresh());
                this._proxy.connect('notify::g-name-owner', () => this._refresh());
                this._refresh();
            });
    }

    _cached(name) {
        const variant = this._proxy?.get_cached_property(name);
        return variant ? variant.deepUnpack() : {};
    }

    _refresh() {
        this._alive = this._proxy?.g_name_owner != null;
        const values = this._alive ? this._cached('Values') : {};
        this._names = this._alive ? this._cached('Names') : {};
        this._details = this._alive ? this._cached('Details') : {};

        for (const [key, gauge] of this._gauges)
            gauge.setValue(key in values ? values[key] : null);

        if (this._hoveredGauge)
            this._updateTooltip(this._hoveredGauge);
    }

    _activateApp() {
        const app = Shell.AppSystem.get_default().lookup_app(APP_DESKTOP_ID);
        if (app)
            app.activate();
        else
            console.warn(`SysMo Gauges: ${APP_DESKTOP_ID} is not installed`);
    }

    _onGaugeHover(gauge) {
        if (gauge.hover) {
            this._hoveredGauge = gauge;
            this._updateTooltip(gauge);
            this._showTooltip(gauge);
        } else if (this._hoveredGauge === gauge) {
            this._hoveredGauge = null;
            this._hideTooltip();
        }
    }

    _updateTooltip(gauge) {
        const {spec, value} = gauge;
        const escape = text => GLib.markup_escape_text(String(text), -1);

        let markup;
        if (!this._alive) {
            markup = `<b>${escape(spec.title)}</b>\n` +
                '<span size="small" alpha="65%">SysMo is not running · click to start it</span>';
        } else {
            const reading = value === null ? 'no data' : `${Math.round(value)} ${spec.unit}`;
            markup = `<b>${escape(spec.title)}</b>  ${escape(reading)}`;
            const subtitle = [this._names[spec.device], this._details[spec.key]]
                .filter(Boolean).join(' · ');
            if (subtitle)
                markup += `\n<span size="small" alpha="65%">${escape(subtitle)}</span>`;
        }
        this._tooltip.clutter_text.set_markup(markup);
    }

    _showTooltip(gauge) {
        const [x, y] = gauge.get_transformed_position();
        const [w, h] = gauge.get_transformed_size();
        this._tooltip.opacity = 0;
        this._tooltip.show();

        const [, tooltipWidth] = this._tooltip.get_preferred_width(-1);
        const monitor = Main.layoutManager.primaryMonitor;
        let tx = Math.round(x + w / 2 - tooltipWidth / 2);
        tx = Math.max(monitor.x + 4, Math.min(tx, monitor.x + monitor.width - tooltipWidth - 4));
        this._tooltip.set_position(tx, Math.round(y + h + 6));
        this._tooltip.ease({opacity: 255, duration: 120, mode: Clutter.AnimationMode.EASE_OUT_QUAD});
    }

    _hideTooltip() {
        this._tooltip.ease({
            opacity: 0,
            duration: 100,
            mode: Clutter.AnimationMode.EASE_OUT_QUAD,
            onComplete: () => this._tooltip.hide(),
        });
    }

    _onDestroy() {
        this._proxy = null;
        if (this._tooltip) {
            this._tooltip.destroy();
            this._tooltip = null;
        }
    }
});

// Keeps SysMo's window above the others while the app's "stay-on-top"
// setting (its pin button) is on. GTK 4 apps cannot do this themselves on
// Wayland; the shell can, on X11 and Wayland alike.
class StayOnTop {
    constructor() {
        const schema = Gio.SettingsSchemaSource.get_default()?.lookup(SETTINGS_SCHEMA, true);
        this._app = Shell.AppSystem.get_default().lookup_app(APP_DESKTOP_ID);
        if (!schema?.has_key(STAY_ON_TOP_KEY) || !this._app) {
            console.warn('SysMo Gauges: SysMo is not installed; stay on top is unavailable');
            this._settings = null;
            return;
        }
        this._settings = new Gio.Settings({settings_schema: schema});
        this._settings.connectObject(`changed::${STAY_ON_TOP_KEY}`, () => this._apply(), this);
        // SysMo gets a new window each time it is shown after being closed.
        this._app.connectObject('windows-changed', () => this._apply(), this);
        this._apply();
    }

    _apply(above = this._settings.get_boolean(STAY_ON_TOP_KEY)) {
        for (const window of this._app.get_windows()) {
            if (above && !window.is_above())
                window.make_above();
            else if (!above && window.is_above())
                window.unmake_above();
        }
    }

    destroy() {
        if (!this._settings)
            return;
        this._settings.disconnectObject(this);
        this._app.disconnectObject(this);
        this._apply(false);
        this._settings = null;
    }
}

export default class SysmoGaugesExtension extends Extension {
    enable() {
        this._indicator = new Indicator();
        Main.panel.addToStatusArea(this.uuid, this._indicator, 0, 'right');
        this._stayOnTop = new StayOnTop();
    }

    disable() {
        this._stayOnTop?.destroy();
        this._stayOnTop = null;
        this._indicator?.destroy();
        this._indicator = null;
    }
}
