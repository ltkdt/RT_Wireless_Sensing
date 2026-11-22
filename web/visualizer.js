/**
 * CSI Amplitude Heatmap Visualizer
 * Pure Vanilla JavaScript + HTML Canvas 2D implementation
 */

// --- Colormaps ---
const COLORMAPS = {
    viridis: [
        [0.0,  [68, 1, 84]],
        [0.1,  [72, 35, 116]],
        [0.2,  [64, 67, 135]],
        [0.3,  [52, 94, 141]],
        [0.4,  [41, 120, 142]],
        [0.5,  [32, 144, 140]],
        [0.6,  [34, 167, 132]],
        [0.7,  [68, 190, 112]],
        [0.8,  [121, 209, 81]],
        [0.9,  [189, 222, 38]],
        [1.0,  [253, 231, 37]]
    ],
    plasma: [
        [0.0,  [13, 8, 135]],
        [0.2,  [106, 0, 168]],
        [0.4,  [177, 42, 144]],
        [0.6,  [225, 100, 98]],
        [0.8,  [248, 166, 54]],
        [1.0,  [240, 249, 33]]
    ],
    jet: [
        [0.0,  [0, 0, 128]],
        [0.15, [0, 0, 255]],
        [0.35, [0, 255, 255]],
        [0.65, [255, 255, 0]],
        [0.85, [255, 0, 0]],
        [1.0,  [128, 0, 0]]
    ],
    // Cyclic map for phase (−π…π): both ends share a colour so a wrap is not
    // read as a discontinuity. First and last stops are identical on purpose.
    twilight: [
        [0.0,  [226, 217, 226]],
        [0.15, [160, 139, 187]],
        [0.3,  [91, 96, 158]],
        [0.45, [46, 60, 110]],
        [0.55, [46, 60, 110]],
        [0.65, [91, 96, 158]],
        [0.8,  [160, 139, 187]],
        [1.0,  [226, 217, 226]]
    ],
    // Diverging map used for a normalized amplitude that can go negative.
    rdbu: [
        [0.0,  [5, 48, 97]],
        [0.25, [103, 169, 207]],
        [0.5,  [247, 247, 247]],
        [0.75, [239, 138, 98]],
        [1.0,  [103, 0, 31]]
    ]
};

function interpolateColormap(stops, t) {
    const clamped = Math.max(0, Math.min(1, t));
    let i = 0;
    while (i < stops.length - 1 && stops[i + 1][0] < clamped) {
        i++;
    }
    const [t0, c0] = stops[i];
    const [t1, c1] = stops[i + 1] || stops[i];
    const span = t1 - t0;
    const factor = span > 0 ? (clamped - t0) / span : 0;
    const r = Math.round(c0[0] + factor * (c1[0] - c0[0]));
    const g = Math.round(c0[1] + factor * (c1[1] - c0[1]));
    const b = Math.round(c0[2] + factor * (c1[2] - c0[2]));
    return [r, g, b];
}

// Generate precomputed 256-entry lookup table for colormap
function generateColormapLUT(stops) {
    const lut = new Uint8Array(256 * 3);
    for (let i = 0; i < 256; i++) {
        const t = i / 255.0;
        const [r, g, b] = interpolateColormap(stops, t);
        lut[i * 3 + 0] = r;
        lut[i * 3 + 1] = g;
        lut[i * 3 + 2] = b;
    }
    return lut;
}

// Precomputed lookup tables
const COLORMAP_LUTS = {
    viridis: generateColormapLUT(COLORMAPS.viridis),
    plasma: generateColormapLUT(COLORMAPS.plasma),
    jet: generateColormapLUT(COLORMAPS.jet),
    twilight: generateColormapLUT(COLORMAPS.twilight),
    rdbu: generateColormapLUT(COLORMAPS.rdbu)
};

// Last-resort fallback only: Rust always sends metadata.subcarriers, and the
// static label in index.html keeps "64 subcarriers / frame" as its fallback
// text. All rendering paths must prefer metadata.subcarriers.
const FALLBACK_SUBCARRIERS = 64;

// Nominal PPS assumed when the payload carries no `--pps` argument
// (`metadata.nominal_pps` is `null`/absent). Kept in sync with
// `collection::DEFAULT_PPS` in Rust: the dashboard says it is a default rather
// than presenting it as a measured value.
const DEFAULT_NOMINAL_PPS = 20;

// --- Heatmap Visualizer Class ---
class CsiVisualizer {
    constructor(canvasId) {
        this.canvas = document.getElementById(canvasId);
        this.ctx = this.canvas.getContext('2d');
        this.data = null;
        this.colormap = 'viridis';
        this.smoothing = false;
        this.minAmp = 0;
        this.maxAmp = 100;
        this.hoverState = null;
        this.occupancy = null;
        this.diagnosticView = null;
        this.dspView = null;
        // Set by the bootstrap when the payload came from the live streaming
        // branch; the DSP view uses it to show its "after streaming" note.
        this.live = false;
        this.offscreenCanvas = document.createElement('canvas');
        this.offscreenCtx = this.offscreenCanvas.getContext('2d');

        // Layout Margins (CSS pixels)
        this.margin = {
            top: 55,
            right: 120,
            bottom: 65,
            left: 75
        };

        this.tooltip = document.getElementById('heatmapTooltip');
        this.wrapper = this.canvas ? this.canvas.parentElement : null;
        this.offscreenDirty = true;

        this.initEvents();
    }

    setData(data) {
        this.data = data;
        this.calculateBounds();
        this.updateMetadataUI();
        this.refreshOccupancy();
        this.offscreenDirty = true;
        this.render();
        if (this.diagnosticView) {
            this.diagnosticView.setData(data);
            this.diagnosticView.setSelection(this.occupancy);
        }
        if (this.dspView) {
            this.dspView.setData(data, this.live);
        }
    }

    calculateBounds() {
        if (!this.data || !this.data.amplitudes || this.data.amplitudes.length === 0) return;

        let min = Infinity;
        let max = -Infinity;
        const frames = this.data.amplitudes.length;
        const scCount = this.data.amplitudes[0].length;

        for (let f = 0; f < frames; f++) {
            const row = this.data.amplitudes[f];
            for (let sc = 0; sc < scCount; sc++) {
                const val = row[sc];
                if (val < min) min = val;
                if (val > max) max = val;
            }
        }

        // Clamp reasonable defaults
        this.minAmp = min;
        this.maxAmp = max > min ? max : min + 1.0;
    }

    // Requested (nominal) packets-per-second, i.e. the `--pps` argument.
    // Returns null when the payload carries no argument, so callers can show
    // the fallback instead of pretending it was requested.
    getRequestedPps() {
        const meta = this.data && this.data.metadata;
        if (!meta) return null;
        const configured = meta.nominal_pps;
        if (typeof configured === 'number' && Number.isFinite(configured) && configured > 0) {
            return configured;
        }
        return null;
    }

    // Nominal packets-per-second used to divide a one-second interval into
    // occupancy slots: the `--pps` argument when present, otherwise the
    // default rate. The measured rate is never used here — it is reported
    // separately as effective PPS.
    getNominalPps() {
        const requested = this.getRequestedPps();
        const pps = Math.round(requested !== null ? requested : DEFAULT_NOMINAL_PPS);
        return Math.max(1, Math.min(pps || 1, 100000));
    }

    // Subcarrier count per frame. Rust always sends metadata.subcarriers;
    // the fallbacks keep old payloads and the static label working.
    getSubcarrierCount() {
        if (!this.data) return FALLBACK_SUBCARRIERS;
        const meta = this.data.metadata;
        if (meta && Number.isFinite(meta.subcarriers) && meta.subcarriers > 0) {
            return meta.subcarriers;
        }
        if (this.data.amplitudes && this.data.amplitudes[0]) {
            return this.data.amplitudes[0].length;
        }
        return FALLBACK_SUBCARRIERS;
    }

    // Occupancy for the second [secondStart, secondStart + 1):
    // divide the second into N = nominal PPS slots and report the fraction of
    // slots that contain at least one measurement.
    computeOccupancy(secondStart) {
        if (!this.data || !Array.isArray(this.data.timestamps) || this.data.timestamps.length === 0) {
            return null;
        }
        if (!Number.isFinite(secondStart)) return null;

        const slots = this.getNominalPps();
        const filled = new Uint8Array(slots);
        const secondsEnd = secondStart + 1;
        let measurements = 0;

        for (let i = 0; i < this.data.timestamps.length; i++) {
            const t = this.data.timestamps[i];
            if (t >= secondStart && t < secondsEnd) {
                measurements++;
                // Add a tiny epsilon so exact slot boundaries (for example
                // 24.13 s with 100 slots) are not pushed into the previous
                // slot by binary floating-point rounding.
                let slot = Math.floor((t - secondStart) * slots + 1e-6);
                if (slot < 0) slot = 0;
                if (slot >= slots) slot = slots - 1;
                filled[slot] = 1;
            }
        }

        let filledCount = 0;
        for (let i = 0; i < slots; i++) filledCount += filled[i];

        return {
            secondStart,
            slots,
            filled: filledCount,
            measurements,
            occupancy: filledCount / slots
        };
    }

    updateOccupancyUI() {
        const el = document.getElementById('hoverOccupancy');
        if (!el) return;
        if (!this.occupancy) {
            el.textContent = '--';
            return;
        }
        const o = this.occupancy;
        el.textContent = `${(o.occupancy * 100).toFixed(1)}% (${o.filled}/${o.slots} @ ${o.secondStart}s)`;
    }

    // Recompute the selected second after a live data refresh, and drop it once
    // the sliding 30-second window no longer contains that second.
    refreshOccupancy() {
        if (!this.occupancy || !this.data || !Array.isArray(this.data.timestamps) || this.data.timestamps.length === 0) {
            this.updateOccupancyUI();
            return;
        }

        const secondStart = this.occupancy.secondStart;
        const first = this.data.timestamps[0];
        const last = this.data.timestamps[this.data.timestamps.length - 1];

        if (secondStart + 1 <= first || secondStart > last) {
            this.occupancy = null;
        } else {
            this.occupancy = this.computeOccupancy(secondStart);
        }
        this.updateOccupancyUI();
    }

    updateMetadataUI() {
        if (!this.data || !this.data.metadata) return;
        const meta = this.data.metadata;

        const setElem = (id, val) => {
            const el = document.getElementById(id);
            if (el) el.textContent = val;
        };

        setElem('metaStartTime', meta.start_time_formatted || '0.000 s');
        setElem('metaDuration', `${meta.duration_seconds.toFixed(2)} s`);
        setElem('metaFrames', `${meta.total_frames.toLocaleString()} frames`);

        // Packet rate: the requested `--pps` argument (nominal) next to the
        // rate actually measured from the device clock (effective).
        const requested = this.getRequestedPps();
        const hasArgument = requested !== null;
        const nominal = this.getNominalPps();
        setElem('metaNominalRate', `${nominal} PPS`);
        setElem(
            'metaNominalRateHint',
            hasArgument ? 'Requested --pps vs measured rate' : `No --pps argument — default ${nominal} PPS`
        );
        const nominalEl = document.getElementById('metaNominalRate');
        if (nominalEl) {
            nominalEl.classList.toggle('metric-pair-default', !hasArgument);
            nominalEl.title = hasArgument
                ? `--pps ${nominal} (slot grid rate)`
                : `--pps not supplied; DEFAULT_NOMINAL_PPS = ${nominal} is used for the slot grid`;
        }
        setElem(
            'metaRate',
            Number.isFinite(meta.effective_pps) ? `${meta.effective_pps.toFixed(2)} PPS` : '--'
        );

        setElem(
            'metaSubcarriers',
            `${Number.isFinite(meta.subcarriers) && meta.subcarriers > 0 ? meta.subcarriers : FALLBACK_SUBCARRIERS} subcarriers / frame`
        );

        // Classification Card
        const classBadge = document.getElementById('classificationBadge');
        const classDetails = document.getElementById('classificationDetails');
        if (this.data.classification && this.data.classification.predicted_class) {
            const cls = this.data.classification;
            if (classBadge) {
                classBadge.textContent = cls.predicted_class;
                classBadge.style.color = '#34d399';
                classBadge.style.borderColor = 'rgba(52, 211, 153, 0.4)';
                classBadge.style.background = 'rgba(52, 211, 153, 0.15)';
            }
            if (classDetails) {
                classDetails.textContent = cls.confidence !== null && cls.confidence !== undefined
                    ? `Confidence: ${(cls.confidence * 100).toFixed(1)}%`
                    : (cls.details || 'Model prediction');
            }
        } else {
            if (classBadge) classBadge.textContent = 'Standby / None';
            if (classDetails) classDetails.textContent = 'Awaiting inference model';
        }
    }

    updateOffscreenBuffer() {
        if (!this.data || !this.data.amplitudes || this.data.amplitudes.length === 0) return;

        const numFrames = this.data.amplitudes.length;
        const numSubcarriers = this.data.amplitudes[0].length;

        this.offscreenCanvas.width = numFrames;
        this.offscreenCanvas.height = numSubcarriers;

        const imgData = this.offscreenCtx.createImageData(numFrames, numSubcarriers);
        const pixels = imgData.data;
        const lut = COLORMAP_LUTS[this.colormap] || COLORMAP_LUTS.viridis;
        const range = this.maxAmp - this.minAmp;

        // Render with Subcarrier 0 at the bottom (y = numSubcarriers - 1 - sc)
        // so higher subcarriers are rendered upwards, matching standard scientific plots.
        for (let sc = 0; sc < numSubcarriers; sc++) {
            const py = numSubcarriers - 1 - sc;
            for (let f = 0; f < numFrames; f++) {
                const amp = this.data.amplitudes[f][sc];
                const norm = Math.max(0, Math.min(1, (amp - this.minAmp) / range));
                const lutIdx = Math.floor(norm * 255) * 3;

                const pIdx = (py * numFrames + f) * 4;
                pixels[pIdx + 0] = lut[lutIdx + 0];
                pixels[pIdx + 1] = lut[lutIdx + 1];
                pixels[pIdx + 2] = lut[lutIdx + 2];
                pixels[pIdx + 3] = 255;
            }
        }

        this.offscreenCtx.putImageData(imgData, 0, 0);
    }

    render() {
        if (!this.data) return;

        if (this.offscreenDirty) {
            this.updateOffscreenBuffer();
            this.offscreenDirty = false;
        }

        // Canvas sizing for Retina / High DPI displays
        const rect = this.canvas.getBoundingClientRect();
        const dpr = window.devicePixelRatio || 1;
        const width = Math.max(800, rect.width || 1200);
        const height = Math.max(450, Math.round(width * 0.45));

        this.canvas.width = Math.round(width * dpr);
        this.canvas.height = Math.round(height * dpr);
        this.canvas.style.height = `${height}px`;

        this.ctx.save();
        this.ctx.scale(dpr, dpr);

        // Clear background
        this.ctx.fillStyle = '#0b1120';
        this.ctx.fillRect(0, 0, width, height);

        const plotX = this.margin.left;
        const plotY = this.margin.top;
        const plotW = width - this.margin.left - this.margin.right;
        const plotH = height - this.margin.top - this.margin.bottom;

        this.plotBounds = { x: plotX, y: plotY, w: plotW, h: plotH, width, height };

        // 1. Draw Heatmap Matrix
        this.ctx.save();
        this.ctx.beginPath();
        this.ctx.rect(plotX, plotY, plotW, plotH);
        this.ctx.clip();

        this.ctx.imageSmoothingEnabled = this.smoothing;
        this.ctx.drawImage(this.offscreenCanvas, plotX, plotY, plotW, plotH);
        this.ctx.restore();

        // 2. Draw Plot Area Border
        this.ctx.strokeStyle = '#334155';
        this.ctx.lineWidth = 1;
        this.ctx.strokeRect(plotX, plotY, plotW, plotH);

        // 3. Draw Chart Title
        this.ctx.fillStyle = '#f8fafc';
        this.ctx.font = '600 16px -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif';
        this.ctx.textAlign = 'center';
        this.ctx.textBaseline = 'bottom';
        this.ctx.fillText('CSI Amplitude Heatmap', plotX + plotW / 2, plotY - 14);

        // 4. Draw Axes & Ticks
        this.drawAxes(plotX, plotY, plotW, plotH);

        // 5. Draw Colorbar
        this.drawColorbar(plotX + plotW + 35, plotY, 18, plotH);

        // 6. Draw Hover Reticle / Crosshair if active
        if (this.hoverState) {
            this.drawHoverReticle(plotX, plotY, plotW, plotH);
        }

        this.ctx.restore();
    }

    drawAxes(px, py, pw, ph) {
        const timestamps = this.data.timestamps || [];
        const tMin = timestamps.length ? timestamps[0] : 0;
        const tMax = timestamps.length
            ? timestamps[timestamps.length - 1]
            : (this.data.metadata.duration_seconds || 0);
        const timeSpan = Math.max(tMax - tMin, 1e-9);
        const subcarrierCount = this.getSubcarrierCount();

        this.ctx.fillStyle = '#94a3b8';
        this.ctx.font = '11px ui-monospace, SFMono-Regular, Menlo, monospace';

        // X-Axis (Time in seconds)
        this.ctx.textAlign = 'center';
        this.ctx.textBaseline = 'top';

        // Compute sensible time step
        const approxTicks = 8;
        const rawStep = timeSpan / approxTicks;
        const niceSteps = [0.1, 0.2, 0.5, 1, 2, 5, 10, 15, 20, 30];
        let step = niceSteps[niceSteps.length - 1];
        for (const s of niceSteps) {
            if (rawStep <= s) {
                step = s;
                break;
            }
        }

        this.ctx.strokeStyle = '#1e293b';
        this.ctx.lineWidth = 1;

        // Keep labels at their absolute elapsed times, so a sliding window at
        // 32 s shows 2..32 instead of resetting to 0 on every refresh.
        const tickCount = Math.max(1, Math.floor(timeSpan / step + 1e-9));
        for (let i = 0; i <= tickCount; i++) {
            const t = tMin + i * step;
            if (t > tMax + 1e-6) break;
            const frac = (t - tMin) / timeSpan;
            const x = px + frac * pw;

            // Tick mark
            this.ctx.beginPath();
            this.ctx.moveTo(x, py + ph);
            this.ctx.lineTo(x, py + ph + 5);
            this.ctx.strokeStyle = '#475569';
            this.ctx.stroke();

            // Gridline (subtle)
            if (frac > 0 && frac < 1) {
                this.ctx.beginPath();
                this.ctx.moveTo(x, py);
                this.ctx.lineTo(x, py + ph);
                this.ctx.strokeStyle = 'rgba(51, 65, 85, 0.35)';
                this.ctx.stroke();
            }

            // Label
            this.ctx.fillText(`${t.toFixed(step < 1 ? 1 : 0)}`, x, py + ph + 8);
        }

        // X-Axis Label
        this.ctx.fillStyle = '#f8fafc';
        this.ctx.font = '500 13px -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif';
        this.ctx.fillText('Time (s)', px + pw / 2, py + ph + 34);

        // Y-Axis (Subcarrier Index 0..63)
        this.ctx.fillStyle = '#94a3b8';
        this.ctx.font = '11px ui-monospace, SFMono-Regular, Menlo, monospace';
        this.ctx.textAlign = 'right';
        this.ctx.textBaseline = 'middle';

        const yTicks = [0, 10, 20, 30, 40, 50, 60, subcarrierCount - 1];
        for (const sc of yTicks) {
            const frac = sc / (subcarrierCount - 1);
            // sc=0 is at bottom (py + ph), sc=63 is at top (py)
            const y = py + ph - frac * ph;

            // Tick mark
            this.ctx.beginPath();
            this.ctx.moveTo(px - 5, y);
            this.ctx.lineTo(px, y);
            this.ctx.strokeStyle = '#475569';
            this.ctx.stroke();

            // Gridline
            if (frac > 0 && frac < 1) {
                this.ctx.beginPath();
                this.ctx.moveTo(px, y);
                this.ctx.lineTo(px + pw, y);
                this.ctx.strokeStyle = 'rgba(51, 65, 85, 0.35)';
                this.ctx.stroke();
            }

            // Label
            this.ctx.fillText(`${sc}`, px - 8, y);
        }

        // Y-Axis Label (Rotated)
        this.ctx.save();
        this.ctx.translate(px - 48, py + ph / 2);
        this.ctx.rotate(-Math.PI / 2);
        this.ctx.fillStyle = '#f8fafc';
        this.ctx.font = '500 13px -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif';
        this.ctx.textAlign = 'center';
        this.ctx.textBaseline = 'bottom';
        this.ctx.fillText('Subcarrier Index', 0, 0);
        this.ctx.restore();
    }

    drawColorbar(cx, cy, cw, ch) {
        // Vertical gradient offscreen
        const gradientCanvas = document.createElement('canvas');
        gradientCanvas.width = 1;
        gradientCanvas.height = 256;
        const gCtx = gradientCanvas.getContext('2d');
        const img = gCtx.createImageData(1, 256);
        const lut = COLORMAP_LUTS[this.colormap] || COLORMAP_LUTS.viridis;

        for (let i = 0; i < 256; i++) {
            // High values at top (y=0), low values at bottom (y=255)
            const lutVal = (255 - i) * 3;
            img.data[i * 4 + 0] = lut[lutVal + 0];
            img.data[i * 4 + 1] = lut[lutVal + 1];
            img.data[i * 4 + 2] = lut[lutVal + 2];
            img.data[i * 4 + 3] = 255;
        }
        gCtx.putImageData(img, 0, 0);

        // Draw colorbar
        this.ctx.drawImage(gradientCanvas, cx, cy, cw, ch);
        this.ctx.strokeStyle = '#334155';
        this.ctx.lineWidth = 1;
        this.ctx.strokeRect(cx, cy, cw, ch);

        // Colorbar Ticks and Labels
        this.ctx.fillStyle = '#94a3b8';
        this.ctx.font = '11px ui-monospace, SFMono-Regular, Menlo, monospace';
        this.ctx.textAlign = 'left';
        this.ctx.textBaseline = 'middle';

        const tickCount = 6;
        const range = this.maxAmp - this.minAmp;
        for (let i = 0; i <= tickCount; i++) {
            const frac = i / tickCount;
            const y = cy + ch - frac * ch;
            const val = this.minAmp + frac * range;

            this.ctx.beginPath();
            this.ctx.moveTo(cx + cw, y);
            this.ctx.lineTo(cx + cw + 4, y);
            this.ctx.strokeStyle = '#475569';
            this.ctx.stroke();

            this.ctx.fillText(`${val.toFixed(1)}`, cx + cw + 7, y);
        }

        // Colorbar Label (Rotated on outer right)
        this.ctx.save();
        this.ctx.translate(cx + cw + 46, cy + ch / 2);
        this.ctx.rotate(Math.PI / 2);
        this.ctx.fillStyle = '#f8fafc';
        this.ctx.font = '500 12px -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif';
        this.ctx.textAlign = 'center';
        this.ctx.textBaseline = 'bottom';
        this.ctx.fillText('Amplitude (a.u.)', 0, 0);
        this.ctx.restore();
    }

    drawHoverReticle(px, py, pw, ph) {
        if (!this.hoverState) return;
        const { x, y } = this.hoverState;

        // Crosshairs
        this.ctx.save();
        this.ctx.strokeStyle = 'rgba(255, 255, 255, 0.45)';
        this.ctx.lineWidth = 1;
        this.ctx.setLineDash([4, 4]);

        // Vertical line
        this.ctx.beginPath();
        this.ctx.moveTo(x, py);
        this.ctx.lineTo(x, py + ph);
        this.ctx.stroke();

        // Horizontal line
        this.ctx.beginPath();
        this.ctx.moveTo(px, y);
        this.ctx.lineTo(px + pw, y);
        this.ctx.stroke();

        this.ctx.restore();
    }

    eventToPoint(e) {
        if (!this.data || !this.plotBounds || !this.data.amplitudes || this.data.amplitudes.length === 0) {
            return null;
        }

        const rect = this.canvas.getBoundingClientRect();
        const mouseX = e.clientX - rect.left;
        const mouseY = e.clientY - rect.top;
        const { x: px, y: py, w: pw, h: ph } = this.plotBounds;

        if (mouseX < px || mouseX > px + pw || mouseY < py || mouseY > py + ph) {
            return null;
        }

        const numFrames = this.data.amplitudes.length;
        const numSubcarriers = this.getSubcarrierCount();

        const fracX = (mouseX - px) / pw;
        const fracY = (mouseY - py) / ph;
        const frameIdx = Math.min(numFrames - 1, Math.max(0, Math.floor(fracX * numFrames)));
        // Inverted Y: bottom is 0, top is numSubcarriers - 1
        const scIdx = Math.min(numSubcarriers - 1, Math.max(0, Math.floor((1 - fracY) * numSubcarriers)));

        const timeSec = this.data.timestamps && this.data.timestamps[frameIdx] !== undefined
            ? this.data.timestamps[frameIdx]
            : (frameIdx / numFrames) * this.data.metadata.duration_seconds;
        const ampVal = this.data.amplitudes[frameIdx][scIdx];

        return {
            mouseX,
            mouseY,
            clientX: e.clientX,
            clientY: e.clientY,
            frameIdx,
            scIdx,
            timeSec,
            ampVal
        };
    }

    updateHoverReadout(point) {
        this.hoverState = {
            x: point.mouseX,
            y: point.mouseY,
            frameIdx: point.frameIdx,
            scIdx: point.scIdx,
            timeSec: point.timeSec,
            ampVal: point.ampVal
        };

        document.getElementById('hoverTime').textContent = `${point.timeSec.toFixed(3)} s`;
        document.getElementById('hoverSubcarrier').textContent = `${point.scIdx}`;
        document.getElementById('hoverAmplitude').textContent = `${point.ampVal.toFixed(2)}`;
        document.getElementById('hoverFrame').textContent = `#${point.frameIdx}`;

        const tooltip = this.tooltip || document.getElementById('heatmapTooltip');
        if (tooltip) {
            tooltip.innerHTML = `
                <div class="tip-title"><strong>Time:</strong> ${point.timeSec.toFixed(3)} s (#${point.frameIdx})</div>
                <div><strong>Subcarrier:</strong> ${point.scIdx}</div>
                <div><strong>Amplitude:</strong> ${point.ampVal.toFixed(3)} a.u.</div>
            `;
            tooltip.style.display = 'block';

            const wrap = this.wrapper || tooltip.parentElement;
            if (wrap) {
                const wrapRect = wrap.getBoundingClientRect();
                let left = point.clientX - wrapRect.left + 15;
                let top = point.clientY - wrapRect.top + 15;
                const tw = tooltip.offsetWidth;
                const th = tooltip.offsetHeight;
                if (left + tw > wrapRect.width - 8) left = point.clientX - wrapRect.left - tw - 10;
                if (top + th > wrapRect.height - 8) top = point.clientY - wrapRect.top - th - 10;
                tooltip.style.left = `${Math.max(4, left)}px`;
                tooltip.style.top = `${Math.max(4, top)}px`;
            }
        }
    }

    initEvents() {
        this.canvas.addEventListener('mousemove', (e) => {
            const point = this.eventToPoint(e);
            if (!point) {
                this.clearHover();
                return;
            }
            this.updateHoverReadout(point);
            this.render();
        });

        // Clicking a heatmap point computes occupancy for that point's second.
        this.canvas.addEventListener('click', (e) => {
            const point = this.eventToPoint(e);
            if (!point) return;

            this.updateHoverReadout(point);
            this.occupancy = this.computeOccupancy(Math.floor(point.timeSec));
            this.updateOccupancyUI();
            // The diagnostic view reuses this same click-to-select-second.
            if (this.diagnosticView) this.diagnosticView.setSelection(this.occupancy);
            this.render();
        });

        this.canvas.addEventListener('mouseleave', () => {
            this.clearHover();
        });

        window.addEventListener('resize', () => {
            if (this.data) this.render();
        });

        // Controls
        const colormapSelect = document.getElementById('colormapSelect');
        if (colormapSelect) {
            colormapSelect.addEventListener('change', (e) => {
                this.colormap = e.target.value;
                this.offscreenDirty = true;
                this.render();
                if (this.diagnosticView) this.diagnosticView.render();
            });
        }

        const smoothingToggle = document.getElementById('smoothingToggle');
        if (smoothingToggle) {
            smoothingToggle.addEventListener('change', (e) => {
                this.smoothing = e.target.checked;
                this.offscreenDirty = true;
                this.render();
            });
        }

        const exportBtn = document.getElementById('exportBtn');
        if (exportBtn) {
            exportBtn.addEventListener('click', () => {
                const link = document.createElement('a');
                link.download = `csi_amplitude_heatmap_${Date.now()}.png`;
                link.href = this.canvas.toDataURL('image/png');
                link.click();
            });
        }
    }

    clearHover() {
        this.hoverState = null;
        const tooltip = this.tooltip || document.getElementById('heatmapTooltip');
        if (tooltip) tooltip.style.display = 'none';

        document.getElementById('hoverTime').textContent = '--';
        document.getElementById('hoverSubcarrier').textContent = '--';
        document.getElementById('hoverAmplitude').textContent = '--';
        document.getElementById('hoverFrame').textContent = '--';
        this.render();
    }
}

// --- Diagnostic View ---
//
// Separate canvas behind a bottom toggle, independent of the amplitude
// heatmap (which is untouched while the toggle is off). The toggle is only
// offered when the payload carries a `diagnostics` field; old payloads keep
// it hidden. Rust assigns every record to a slot and precomputes the deltas,
// the empty-slot classification, and the per-second statistics, so this view
// only formats numbers: filled columns use the amplitude colormap, empty
// columns render hatched gray.

// Empty-slot causes, as serialized by `DiagnosticCause` in Rust.
const CAUSE_LABELS = {
    device_drop: 'dropped at the device stream layer — RX (device)',
    desktop_backpressure: 'the desktop did not accept the batch — desktop reads too slowly',
    air_loss: 'lost over the air — TX (AP) → RX airtime',
    generator_missed: 'no record generated — generator (desktop) or firmware',
    ambiguous: 'cannot attribute — no boundary record on one side'
};

const CAUSE_LAYERS = {
    device_drop: 'rx',
    desktop_backpressure: 'desktop',
    air_loss: 'tx',
    generator_missed: 'generator',
    ambiguous: ''
};

// Fixed colour per cause, shared by the donut slices and the legend swatches.
const CAUSE_COLORS = {
    device_drop: '#f87171',
    desktop_backpressure: '#fbbf24',
    air_loss: '#c084fc',
    generator_missed: '#38bdf8',
    ambiguous: '#64748b'
};

// One-line meanings for the capture-overview panel, rendered into `data-tip`
// attributes and shown by the delegated tooltip listener.
const OVERVIEW_TIPS = {
    summary: 'diagnostics.summary — capture-level aggregation of all empty slots between the first and last received record, precomputed in Rust',
    loss_ratio: 'empty slots / total covered slots over the whole capture',
    device_drop: 'Δstream gap + Δraw_drop_total > 0 — RX device dropped the record before sending (queue full / invalid / batch failed)',
    desktop_backpressure: 'Δstream gap + Δbackpressure > 0 — desktop client did not accept the batch; measured at the device socket, caused on the desktop',
    air_loss: 'contiguous counters + neighbouring RSSI ≤ LOW_RSSI_THRESHOLD_DBM (−70 dBm) — lost over the air, TX (AP) → RX',
    generator_missed: 'contiguous counters + normal RSSI — marker not sent / firmware produced no CSI',
    ambiguous: 'no boundary record closes the gap — capture end or live window edge',
    dropped_frames_total: 'Σ (Δstream − 1): packets offered to the stream but never received',
    delta_drop_total: 'total Δraw_drop_total — RX-side drops (records never left the device)',
    delta_backpressure_total: 'total Δraw_send_backpressure_total — batches the desktop did not accept',
    filled_records: 'records received inside the covered span',
    empty_slots: 'slots inside the covered span without a record',
    total_slots: 'covered span in slots: filled records + empty slots',
    empty_slots_low_rssi: 'empty slots classified as over-air loss (low neighbouring RSSI)'
};

// Code tables mirrored from `csi_header.rs`.
const PHY_NAMES = {
    0: 'unknown', 1: 'legacy', 2: 'ht', 3: 'vht', 4: 'he-su', 5: 'he-mu', 6: 'he-ersu', 7: 'he-tb'
};
const WIDTH_NAMES = { 0: 'unknown', 1: '20', 2: '40', 3: '80', 4: '160', 5: '80+80' };
const FLAG_LABELS = [
    [1, 'first-word-invalid'],
    [2, 'rx-ts-valid'],
    [4, 'rx-start-ts-valid'],
    [8, 'fresh']
];

// `null`/`undefined` render as an em dash instead of "null".
function orDash(value) {
    return value === null || value === undefined ? '—' : value;
}

function flagLabels(flags) {
    const labels = FLAG_LABELS.filter(([bit]) => (flags & bit) !== 0).map(([, label]) => label);
    return labels.length > 0 ? labels.join(' · ') : 'none';
}

function boundaryText(boundary) {
    return boundary
        ? `#${boundary.frame} · seq ${boundary.seq_num} · ${boundary.device_ticks_us} µs · ${boundary.rssi_dbm} dBm`
        : '— (none in this trace)';
}

class DiagnosticView {
    constructor(visualizer) {
        this.visualizer = visualizer;
        this.canvas = document.getElementById('slotCanvas');
        this.ctx = this.canvas ? this.canvas.getContext('2d') : null;
        this.tooltip = document.getElementById('slotTooltip');
        this.section = document.getElementById('diagnosticSection');
        this.toggleBar = document.getElementById('diagnosticToggleBar');
        this.toggle = document.getElementById('diagnosticToggle');
        this.wrapper = document.getElementById('slotCanvasWrapper');
        this.statsBox = document.getElementById('diagStatsBox');
        this.detailsBox = document.getElementById('diagDetailsBox');
        this.legend = document.getElementById('slotLegend');
        this.legendFilled = document.getElementById('legendFilledSwatch');
        this.hint = document.getElementById('diagnosticHint');
        // Capture-overview panel (above the slot grid).
        this.overviewBox = document.getElementById('diagOverviewBox');
        this.overviewCanvas = document.getElementById('overviewCanvas');
        this.overviewCtx = this.overviewCanvas ? this.overviewCanvas.getContext('2d') : null;
        this.overviewKpis = document.getElementById('overviewKpis');
        this.overviewLegend = document.getElementById('overviewLegend');
        this.overviewTooltip = document.getElementById('overviewTooltip');
        this.data = null;
        this.enabled = false;
        this.selection = null;  // occupancy object of the selected second
        this.slotGrid = null;   // { secondStart, slots, grid: (frameIdx|null)[] }
        this.selectedCell = null; // { slot, sc } clicked cell shown in the details box
        this.plotBounds = null;
        this.margin = { top: 44, right: 18, bottom: 50, left: 58 };
        this.offscreen = document.createElement('canvas');
        this.offCtx = this.offscreen.getContext('2d');
        this.initEvents();
    }

    initEvents() {
        if (this.toggle) {
            this.toggle.addEventListener('change', (e) => this.setEnabled(e.target.checked));
        }
        if (this.canvas) {
            this.canvas.addEventListener('mousemove', (e) => this.onMouseMove(e));
            this.canvas.addEventListener('mouseleave', () => this.hideTooltip());
            this.canvas.addEventListener('click', (e) => this.onSlotClick(e));
        }
        // One delegated listener for the whole overview panel: every KPI row,
        // legend row and the info glyph carries a `data-tip` key.
        if (this.overviewBox) {
            this.overviewBox.addEventListener('mouseover', (e) => this.onOverviewOver(e));
            this.overviewBox.addEventListener('mouseout', (e) => this.onOverviewOut(e));
        }
        window.addEventListener('resize', () => this.render());
    }

    // The toggle is only offered when the payload has a `diagnostics` field.
    setData(data) {
        this.data = data;
        const hasDiagnostics = !!(data && data.diagnostics);
        if (this.toggleBar) this.toggleBar.hidden = !hasDiagnostics;
        if (!hasDiagnostics) this.setEnabled(false);
        this.render();
    }

    // Reuses the heatmap's click-to-select-second (the occupancy object).
    setSelection(occupancy) {
        const previousSecond = this.selection ? this.selection.secondStart : null;
        const nextSecond = occupancy ? occupancy.secondStart : null;
        if (previousSecond !== nextSecond) this.selectedCell = null;
        this.selection = occupancy;
        this.render();
    }

    setEnabled(on) {
        this.enabled = on;
        if (this.toggle) this.toggle.checked = on;
        if (this.section) this.section.hidden = !on;
        if (!on) this.hideTooltip();
        this.render();
    }

    // Column count of the slot grid: Rust rounds the nominal PPS once and
    // passes it explicitly in the diagnostics payload.
    slotsPerSecond() {
        const diag = this.data && this.data.diagnostics;
        if (diag && Number.isFinite(diag.slots_per_second) && diag.slots_per_second > 0) {
            return Math.round(diag.slots_per_second);
        }
        return this.visualizer.getNominalPps();
    }

    // Slot grid of the selected second. Rust assigns every record to a slot
    // (`second` + `slot` in the diagnostics payload), so the grid is read
    // straight from the trace; payloads without records fall back to the same
    // arithmetic as computeOccupancy: slot = floor((t - secondStart) * slots + 1e-6).
    computeSlotGrid(secondStart) {
        const slots = this.slotsPerSecond();
        const second = Math.round(secondStart);
        const grid = new Array(slots).fill(null);
        const diag = this.data && this.data.diagnostics;

        if (diag && Array.isArray(diag.records) && diag.records.length > 0) {
            for (const record of diag.records) {
                if (record.second !== second) continue;
                if (record.slot >= 0 && record.slot < slots && grid[record.slot] === null) {
                    grid[record.slot] = record.frame;
                }
            }
            return { secondStart, second, slots, grid };
        }

        const timestamps = this.data.timestamps || [];
        for (let i = 0; i < timestamps.length; i++) {
            const t = timestamps[i];
            if (t >= secondStart && t < secondStart + 1) {
                let slot = Math.floor((t - secondStart) * slots + 1e-6);
                if (slot < 0) slot = 0;
                if (slot >= slots) slot = slots - 1;
                if (grid[slot] === null) grid[slot] = i;
            }
        }
        return { secondStart, second, slots, grid };
    }

    // Record of one (second, slot) cell, or null when the slot is empty.
    recordAt(second, slot) {
        const diag = this.data && this.data.diagnostics;
        if (!diag || !Array.isArray(diag.records)) return null;
        return diag.records.find((r) => r.second === second && r.slot === slot) || null;
    }

    // Rust classification of one empty slot, or null when it is not in the trace.
    emptySlotAt(second, slot) {
        const diag = this.data && this.data.diagnostics;
        if (!diag || !Array.isArray(diag.empty_slots)) return null;
        return diag.empty_slots.find((e) => e.second === second && e.slot === slot) || null;
    }

    // Per-second statistics computed in Rust.
    secondStats(second) {
        const diag = this.data && this.data.diagnostics;
        if (!diag || !Array.isArray(diag.seconds)) return null;
        return diag.seconds.find((s) => s.second === second) || null;
    }

    // Capture-level summary: the Rust `diagnostics.summary` block, or a
    // client-side aggregation of `empty_slots` for payloads written before the
    // block existed. Independent of the selected second.
    overviewSummary() {
        const diag = this.data && this.data.diagnostics;
        if (!diag) return null;
        if (diag.summary && typeof diag.summary === 'object') return diag.summary;

        // Fallback: aggregate the empty slots the payload already carries.
        const counts = {
            device_drop: 0, desktop_backpressure: 0, air_loss: 0,
            generator_missed: 0, ambiguous: 0
        };
        let dropped = 0;
        let drop = 0;
        let backpressure = 0;
        let lowRssi = 0;
        const empties = Array.isArray(diag.empty_slots) ? diag.empty_slots : [];
        for (const empty of empties) {
            if (counts[empty.cause] !== undefined) counts[empty.cause] += 1;
            dropped += empty.dropped_frames || 0;
            drop += empty.delta_drop || 0;
            backpressure += empty.delta_backpressure || 0;
            if (empty.cause === 'air_loss') lowRssi += 1;
        }
        const filled = Array.isArray(diag.records) ? diag.records.length : 0;
        const total = filled + empties.length;
        return {
            total_slots: total,
            filled_records: filled,
            empty_slots: empties.length,
            loss_ratio: total > 0 ? empties.length / total : 0,
            cause_counts: counts,
            dropped_frames_total: dropped,
            delta_drop_total: drop,
            delta_backpressure_total: backpressure,
            empty_slots_low_rssi: lowRssi
        };
    }

    // Renders the capture overview: KPI rows + donut of the cause split.
    renderOverview() {
        if (!this.overviewBox) return;
        const summary = this.enabled ? this.overviewSummary() : null;
        this.overviewBox.hidden = !summary;
        if (!summary) return;

        const counts = summary.cause_counts || {};
        const emptyTotal = summary.empty_slots || 0;
        const pct = (value) => (emptyTotal > 0 ? (value / emptyTotal) * 100 : 0);

        if (this.overviewKpis) {
            const kpis = [
                ['records / slots', `${summary.filled_records} / ${summary.total_slots}`, 'filled_records'],
                ['loss', `${(summary.loss_ratio * 100).toFixed(1)} %`, 'loss_ratio'],
                ['empty slots', `${summary.empty_slots}`, 'empty_slots'],
                ['dropped frames', `${summary.dropped_frames_total}`, 'dropped_frames_total'],
                ['Δraw_drop_total', `${summary.delta_drop_total}`, 'delta_drop_total'],
                ['Δbackpressure', `${summary.delta_backpressure_total}`, 'delta_backpressure_total']
            ];
            this.overviewKpis.innerHTML = kpis
                .map(([label, value, tip]) => `<div class="diag-kpi" data-tip="${tip}">`
                    + `<span class="kpi-label">${label}</span>`
                    + `<span class="kpi-value">${value}</span></div>`)
                .join('');
        }

        if (this.overviewLegend) {
            const order = ['device_drop', 'desktop_backpressure', 'air_loss', 'generator_missed', 'ambiguous'];
            this.overviewLegend.innerHTML = order
                .map((cause) => {
                    const value = counts[cause] || 0;
                    const layer = CAUSE_LAYERS[cause];
                    const tag = layer ? ` <span class="tip-layer ${layer}">${layer}</span>` : '';
                    return `<div class="diag-overview-legend-row" data-tip="${cause}">`
                        + `<span class="legend-swatch" style="background:${CAUSE_COLORS[cause]}"></span>`
                        + `<span class="overview-legend-label">${CAUSE_LABELS[cause]}${tag}</span>`
                        + `<span class="overview-legend-value">${value} · ${pct(value).toFixed(1)} %</span>`
                        + `</div>`;
                })
                .join('');
        }

        this.drawOverviewDonut(counts, emptyTotal, summary);
    }

    // Donut of the cause split. Slices are drawn from the top, clockwise, in a
    // fixed order so the colours stay stable across polls.
    drawOverviewDonut(counts, emptyTotal, summary) {
        if (!this.overviewCanvas || !this.overviewCtx) return;
        const canvas = this.overviewCanvas;
        const ctx = this.overviewCtx;
        const dpr = window.devicePixelRatio || 1;
        const size = 200;
        canvas.width = Math.round(size * dpr);
        canvas.height = Math.round(size * dpr);
        canvas.style.width = `${size}px`;
        canvas.style.height = `${size}px`;

        ctx.save();
        ctx.scale(dpr, dpr);
        ctx.clearRect(0, 0, size, size);

        const cx = size / 2;
        const cy = size / 2;
        const outer = size / 2 - 6;
        const inner = outer * 0.58;

        if (emptyTotal === 0) {
            ctx.beginPath();
            ctx.arc(cx, cy, (outer + inner) / 2, 0, Math.PI * 2);
            ctx.strokeStyle = '#334155';
            ctx.lineWidth = outer - inner;
            ctx.stroke();
            ctx.fillStyle = '#94a3b8';
            ctx.font = '600 12px ui-monospace, monospace';
            ctx.textAlign = 'center';
            ctx.textBaseline = 'middle';
            ctx.fillText('no empty slots', cx, cy);
            ctx.restore();
            return;
        }

        const order = ['device_drop', 'desktop_backpressure', 'air_loss', 'generator_missed', 'ambiguous'];
        let start = -Math.PI / 2;
        for (const cause of order) {
            const value = counts[cause] || 0;
            if (value <= 0) continue;
            const sweep = (value / emptyTotal) * Math.PI * 2;
            ctx.beginPath();
            ctx.arc(cx, cy, outer, start, start + sweep);
            ctx.arc(cx, cy, inner, start + sweep, start, true);
            ctx.closePath();
            ctx.fillStyle = CAUSE_COLORS[cause];
            ctx.fill();
            start += sweep;
        }

        // Centre readout: the loss ratio is the headline number.
        const total = (summary.total_slots || 0) || (emptyTotal + (summary.filled_records || 0));
        const lossPct = total > 0 ? (emptyTotal / total) * 100 : 0;
        ctx.fillStyle = '#e2e8f0';
        ctx.font = '700 20px ui-monospace, monospace';
        ctx.textAlign = 'center';
        ctx.textBaseline = 'middle';
        ctx.fillText(`${lossPct.toFixed(0)}%`, cx, cy - 6);
        ctx.fillStyle = '#94a3b8';
        ctx.font = '600 10px ui-monospace, monospace';
        ctx.fillText('empty', cx, cy + 12);
        ctx.restore();
    }

    // Delegated hover: any element with `data-tip` inside the overview panel.
    onOverviewOver(event) {
        const target = event.target.closest('[data-tip]');
        if (!target || !this.overviewTooltip || !this.overviewBox) return;
        const tip = OVERVIEW_TIPS[target.dataset.tip];
        if (!tip) return;
        this.overviewTooltip.innerHTML = `<div class="tip-title">${target.dataset.tip}</div>`
            + `<div class="tip-body">${tip}</div>`;
        this.overviewTooltip.style.display = 'block';

        const boxRect = this.overviewBox.getBoundingClientRect();
        const targetRect = target.getBoundingClientRect();
        let left = targetRect.left - boxRect.left;
        let top = targetRect.bottom - boxRect.top + 6;
        const tw = this.overviewTooltip.offsetWidth;
        const th = this.overviewTooltip.offsetHeight;
        if (left + tw > boxRect.width - 8) left = boxRect.width - tw - 8;
        if (top + th > boxRect.height - 8) top = targetRect.top - boxRect.top - th - 6;
        this.overviewTooltip.style.left = `${Math.max(4, left)}px`;
        this.overviewTooltip.style.top = `${Math.max(4, top)}px`;
    }

    onOverviewOut(event) {
        if (!this.overviewTooltip) return;
        const target = event.target.closest('[data-tip]');
        if (!target) return;
        const next = event.relatedTarget;
        if (next && target.contains(next)) return;
        this.overviewTooltip.style.display = 'none';
    }

    render() {
        if (!this.canvas || !this.ctx) return;
        const hasSelection = this.enabled && !!this.data && !!this.selection
            && Array.isArray(this.data.timestamps) && this.data.timestamps.length > 0;

        // The capture overview covers the whole trace, so it renders as soon as
        // the toggle is on, with or without a selected second.
        this.renderOverview();

        if (this.wrapper) this.wrapper.hidden = !hasSelection;
        if (this.legend) this.legend.hidden = !hasSelection;
        if (this.statsBox) this.statsBox.hidden = !hasSelection;

        if (!hasSelection) {
            this.slotGrid = null;
            if (this.hint) {
                this.hint.textContent = this.enabled
                    ? 'Click the heatmap above to select a second.'
                    : '';
            }
            this.renderDetails();
            return;
        }

        this.slotGrid = this.computeSlotGrid(this.selection.secondStart);
        if (this.hint) {
            const filled = this.slotGrid.grid.filter((value) => value !== null).length;
            this.hint.textContent =
                `Selected second ${this.selection.secondStart} s — ${filled}/${this.slotGrid.slots} slots filled.`;
        }
        this.drawSlotGrid();
        this.renderStats();
        this.renderDetails();
    }

    drawSlotGrid() {
        const { slots, grid } = this.slotGrid;
        const scCount = this.visualizer.getSubcarrierCount();
        const amps = this.data.amplitudes;

        // Offscreen buffer: one pixel per (slot, subcarrier); subcarrier 0 at
        // the bottom row, matching the main heatmap orientation.
        this.offscreen.width = slots;
        this.offscreen.height = scCount;
        const img = this.offCtx.createImageData(slots, scCount);
        const px = img.data;
        const lut = COLORMAP_LUTS[this.visualizer.colormap] || COLORMAP_LUTS.viridis;
        const range = (this.visualizer.maxAmp - this.visualizer.minAmp) || 1;
        const EMPTY = [51, 65, 85];

        for (let s = 0; s < slots; s++) {
            const frameIdx = grid[s];
            for (let sc = 0; sc < scCount; sc++) {
                const py = scCount - 1 - sc;
                const pIdx = (py * slots + s) * 4;
                if (frameIdx === null) {
                    px[pIdx] = EMPTY[0];
                    px[pIdx + 1] = EMPTY[1];
                    px[pIdx + 2] = EMPTY[2];
                    px[pIdx + 3] = 255;
                } else {
                    const amp = amps[frameIdx][sc];
                    const norm = Math.max(0, Math.min(1, (amp - this.visualizer.minAmp) / range));
                    const lutIdx = Math.floor(norm * 255) * 3;
                    px[pIdx] = lut[lutIdx];
                    px[pIdx + 1] = lut[lutIdx + 1];
                    px[pIdx + 2] = lut[lutIdx + 2];
                    px[pIdx + 3] = 255;
                }
            }
        }
        this.offCtx.putImageData(img, 0, 0);

        const rect = this.canvas.getBoundingClientRect();
        const dpr = window.devicePixelRatio || 1;
        const width = Math.max(600, rect.width || 1200);
        const height = Math.max(220, Math.round(width * 0.25));
        this.canvas.width = Math.round(width * dpr);
        this.canvas.height = Math.round(height * dpr);
        this.canvas.style.height = `${height}px`;

        const ctx = this.ctx;
        ctx.save();
        ctx.scale(dpr, dpr);
        ctx.fillStyle = '#0b1120';
        ctx.fillRect(0, 0, width, height);

        const m = this.margin;
        const plotX = m.left;
        const plotY = m.top;
        const plotW = width - m.left - m.right;
        const plotH = height - m.top - m.bottom;
        this.plotBounds = { x: plotX, y: plotY, w: plotW, h: plotH, width, height };

        // Matrix (nearest-neighbour so individual slots stay visible).
        ctx.save();
        ctx.beginPath();
        ctx.rect(plotX, plotY, plotW, plotH);
        ctx.clip();
        ctx.imageSmoothingEnabled = false;
        ctx.drawImage(this.offscreen, plotX, plotY, plotW, plotH);

        // Hatch empty columns so they render distinctly from filled ones.
        ctx.strokeStyle = 'rgba(148, 163, 184, 0.55)';
        ctx.lineWidth = 1;
        const colW = plotW / slots;
        for (let s = 0; s < slots; s++) {
            if (grid[s] !== null) continue;
            const x0 = plotX + s * colW;
            ctx.save();
            ctx.beginPath();
            ctx.rect(x0, plotY, colW, plotH);
            ctx.clip();
            ctx.beginPath();
            for (let d = -plotH; d < colW + plotH; d += 8) {
                ctx.moveTo(x0 + d, plotY + plotH);
                ctx.lineTo(x0 + d + plotH, plotY);
            }
            ctx.stroke();
            ctx.restore();
        }
        ctx.restore();

        ctx.strokeStyle = '#334155';
        ctx.lineWidth = 1;
        ctx.strokeRect(plotX, plotY, plotW, plotH);

        // Title
        const filled = grid.filter((v) => v !== null).length;
        ctx.fillStyle = '#f8fafc';
        ctx.font = '600 14px -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif';
        ctx.textAlign = 'left';
        ctx.textBaseline = 'bottom';
        ctx.fillText(
            `Slot grid — second ${this.slotGrid.secondStart} s (${filled}/${slots} slots filled)`,
            plotX,
            plotY - 12
        );

        this.drawSlotAxes(plotX, plotY, plotW, plotH, scCount);
        ctx.restore();

        // The legend is DOM above the canvas, so it never covers the matrix;
        // keep its filled swatch on the active colormap.
        if (this.legendFilled) {
            this.legendFilled.style.background =
                `rgb(${lut[255 * 3]}, ${lut[255 * 3 + 1]}, ${lut[255 * 3 + 2]})`;
        }
    }

    drawSlotAxes(px, py, pw, ph, scCount) {
        const slots = this.slotGrid.slots;
        const ctx = this.ctx;

        ctx.fillStyle = '#94a3b8';
        ctx.font = '10px ui-monospace, SFMono-Regular, Menlo, monospace';
        ctx.textAlign = 'center';
        ctx.textBaseline = 'top';

        // X axis: slot indices, thinned out when there are many slots.
        const step = Math.max(1, Math.ceil(slots / 20));
        for (let s = 0; s < slots; s += step) {
            const x = px + ((s + 0.5) / slots) * pw;
            ctx.beginPath();
            ctx.moveTo(x, py + ph);
            ctx.lineTo(x, py + ph + 4);
            ctx.strokeStyle = '#475569';
            ctx.stroke();
            ctx.fillText(`${s}`, x, py + ph + 7);
        }
        ctx.fillStyle = '#f8fafc';
        ctx.font = '500 12px -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif';
        ctx.fillText(`Slot index (of ${slots} per second)`, px + pw / 2, py + ph + 26);

        // Y axis: subcarrier index, 0 at the bottom.
        ctx.fillStyle = '#94a3b8';
        ctx.font = '10px ui-monospace, SFMono-Regular, Menlo, monospace';
        ctx.textAlign = 'right';
        ctx.textBaseline = 'middle';
        const yTicks = scCount > 1 ? [0, Math.floor((scCount - 1) / 2), scCount - 1] : [0];
        for (const sc of yTicks) {
            const frac = scCount > 1 ? sc / (scCount - 1) : 0;
            const y = py + ph - frac * ph;
            ctx.beginPath();
            ctx.moveTo(px - 4, y);
            ctx.lineTo(px, y);
            ctx.strokeStyle = '#475569';
            ctx.stroke();
            ctx.fillText(`${sc}`, px - 7, y);
        }
        ctx.save();
        ctx.translate(px - 40, py + ph / 2);
        ctx.rotate(-Math.PI / 2);
        ctx.fillStyle = '#f8fafc';
        ctx.font = '500 12px -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif';
        ctx.textAlign = 'center';
        ctx.textBaseline = 'bottom';
        ctx.fillText('Subcarrier Index', 0, 0);
        ctx.restore();
    }

    eventToSlot(e) {
        if (!this.slotGrid || !this.plotBounds) return null;
        const rect = this.canvas.getBoundingClientRect();
        const mx = e.clientX - rect.left;
        const my = e.clientY - rect.top;
        const { x: px, y: py, w: pw, h: ph } = this.plotBounds;
        if (mx < px || mx > px + pw || my < py || my > py + ph) return null;
        const slots = this.slotGrid.slots;
        const scCount = this.visualizer.getSubcarrierCount();
        const slot = Math.min(slots - 1, Math.max(0, Math.floor(((mx - px) / pw) * slots)));
        const sc = Math.min(scCount - 1, Math.max(0, Math.floor((1 - (my - py) / ph) * scCount)));
        return { slot, sc, clientX: e.clientX, clientY: e.clientY };
    }

    onMouseMove(e) {
        const hit = this.eventToSlot(e);
        if (!hit) {
            this.hideTooltip();
            return;
        }
        this.showTooltip(hit);
    }

    hideTooltip() {
        if (this.tooltip) this.tooltip.style.display = 'none';
    }

    // One-line cell identity shared by the hover tooltip and the details box.
    cellTitle(slot, sc) {
        const { slots, grid } = this.slotGrid;
        const frameIdx = grid[slot];
        return frameIdx !== null
            ? `Slot ${slot}/${slots - 1} · SC ${sc} — filled (frame #${frameIdx})`
            : `Slot ${slot}/${slots - 1} · SC ${sc} — empty`;
    }

    detailRow(field, meaning, valueHtml, layer) {
        const layerLabel = {
            rx: 'RX device',
            desktop: 'Desktop',
            tx: 'TX/airtime',
            stream: 'Stream',
            generator: 'Generator'
        }[layer] || '';
        const tag = layer ? ` <span class="tip-layer ${layer}">${layerLabel}</span>` : '';
        return `<div class="diag-detail-row"><span class="detail-field">${field}</span>`
            + `<span class="detail-meaning">${meaning}${tag}</span>`
            + `<span class="detail-value">${valueHtml}</span></div>`;
    }

    // Hover tooltip: only the one-line cell identity. The field meanings and
    // values live in the "Timeslot details" box, filled on click.
    showTooltip(hit) {
        if (!this.tooltip || !this.slotGrid) return;
        this.tooltip.innerHTML = `<div class="tip-title">${this.cellTitle(hit.slot, hit.sc)}</div>`;
        this.tooltip.style.display = 'block';
        // Position inside the wrapper (the tooltip is absolutely positioned there).
        const wrapRect = this.wrapper.getBoundingClientRect();
        let left = hit.clientX - wrapRect.left + 15;
        let top = hit.clientY - wrapRect.top + 15;
        const tw = this.tooltip.offsetWidth;
        const th = this.tooltip.offsetHeight;
        if (left + tw > wrapRect.width - 8) left = hit.clientX - wrapRect.left - tw - 10;
        if (top + th > wrapRect.height - 8) top = hit.clientY - wrapRect.top - th - 10;
        this.tooltip.style.left = `${Math.max(4, left)}px`;
        this.tooltip.style.top = `${Math.max(4, top)}px`;
    }

    // Clicking a cell pins its details in the "Timeslot details" box below the
    // canvas; the hover tooltip stays a one-line identity.
    onSlotClick(e) {
        const hit = this.eventToSlot(e);
        if (!hit) return;
        this.selectedCell = { slot: hit.slot, sc: hit.sc };
        this.renderDetails();
    }

    // Box between the canvas and the statistics box: the per-field meanings and
    // values of the clicked cell, styled like the statistics box. Every value
    // comes from the Rust `DiagnosticRecord` / `DiagnosticEmptySlot` rows.
    renderDetails() {
        if (!this.detailsBox) return;
        const visible = this.enabled && !!this.selectedCell && !!this.slotGrid;
        this.detailsBox.hidden = !visible;
        if (!visible) return;

        const { secondStart, second, slots } = this.slotGrid;
        const { slot, sc } = this.selectedCell;
        const record = this.recordAt(second, slot);
        const PH = '<span class="tip-ph">—</span>';

        let html = `<div class="stats-group-title">Timeslot details — ${this.cellTitle(slot, sc)}</div>`;
        if (record) {
            const amp = this.data.amplitudes[record.frame]
                ? this.data.amplitudes[record.frame][sc]
                : null;
            const origin = this.data.diagnostics && Number.isFinite(this.data.diagnostics.origin_ticks)
                ? this.data.diagnostics.origin_ticks
                : 0;
            const relative = (record.device_ticks_us - origin) / 1e6;
            const gap = record.delta_stream !== null && record.delta_stream > 1
                ? ` (gap of ${record.delta_stream - 1})`
                : '';
            html += this.detailRow('record', `run-wide frame index; slot ${slot} of second ${secondStart} s`, `#${record.frame}`);
            html += this.detailRow('seq_num', 'saturated copy of stream_sequence (cross-check only)', `${record.seq_num}`);
            html += this.detailRow(
                'device_ticks_us',
                'device monotonic clock, µs; slot offset = ticks − slot start (sign = late/early)',
                `${record.device_ticks_us} µs (${relative.toFixed(6)} s, offset ${record.slot_offset_us} µs)`
            );
            html += this.detailRow(
                'Δticks / Δwifi_rx_ts_us',
                'intervals vs previous record; stable Δ(driver) with fluctuating Δ(device) ⇒ delay is after the driver',
                `${orDash(record.delta_ticks_us)} µs / ${orDash(record.delta_wifi_rx_us)} µs`
            );
            html += this.detailRow(
                'Δstream_sequence',
                'one-based packets offered to the stream, incl. dropped; a gap = lost at the device stream layer',
                `${orDash(record.delta_stream)}${gap}`,
                'stream'
            );
            html += this.detailRow(
                'Δraw_drop_total',
                'records the device did NOT send — queue full / invalid / batch failed; not a desktop receive problem',
                `${orDash(record.delta_drop)}`,
                'rx'
            );
            html += this.detailRow(
                'Δraw_send_backpressure_total',
                'batches the desktop client did not accept — desktop reads too slowly',
                `${orDash(record.delta_backpressure)}`,
                'desktop'
            );
            html += this.detailRow(
                'rssi_dbm / noise_floor_dbm / SNR',
                'per-record radio quality; low neighbouring RSSI hints at TX/airtime loss',
                `${record.rssi_dbm} / ${orDash(record.noise_floor_dbm)} / ${orDash(record.snr_db)} dB`,
                'tx'
            );
            html += this.detailRow('flags', 'fresh / first-word-invalid / rx-ts-valid labels', flagLabels(record.flags));
            html += this.detailRow(
                'channel / phy / width',
                'normally constant per session; a mid-run change is worth a warning',
                `${record.channel} / ${PHY_NAMES[record.phy_mode_code] || 'unknown'} / ${WIDTH_NAMES[record.channel_width_code] || 'unknown'} MHz`
            );
            html += this.detailRow('amplitude', 'amplitude of the clicked subcarrier', amp !== null ? amp.toFixed(3) : PH);
        } else {
            const slotTime = secondStart + slot / slots;
            const empty = this.emptySlotAt(second, slot);
            html += this.detailRow('slot time', 'theoretical slot time on the device clock, s', slotTime.toFixed(4));
            if (empty) {
                html += this.detailRow(
                    'classification',
                    CAUSE_LABELS[empty.cause] || empty.cause,
                    empty.cause,
                    CAUSE_LAYERS[empty.cause]
                );
                html += this.detailRow(
                    'dropped frames',
                    'frames the stream skipped across the gap (Δstream − 1)',
                    `${empty.dropped_frames}`
                );
                html += this.detailRow(
                    'Δraw_drop_total / Δbackpressure',
                    'device-side drops vs desktop-side receive failures across the gap',
                    `${empty.delta_drop} / ${empty.delta_backpressure}`
                );
                html += this.detailRow(
                    'previous record',
                    'seq_num and device_ticks_us of the record before the slot',
                    boundaryText(empty.previous)
                );
                html += this.detailRow(
                    'next record',
                    'seq_num and device_ticks_us of the record after the slot',
                    boundaryText(empty.next)
                );
                // `capture_end` tells a capture edge from a live window edge, so
                // an unattributable gap is not mistaken for a lost record.
                if (!empty.next || !empty.previous) {
                    const captureEnd = !!(this.data.diagnostics && this.data.diagnostics.capture_end);
                    const edge = !empty.next
                        ? (captureEnd
                            ? 'the capture ends here — no record follows'
                            : 'the live window ends here — the next record is outside the window')
                        : 'the trace starts here — no record precedes the slot';
                    html += this.detailRow('edge', 'why the gap cannot be attributed', edge);
                }
            } else {
                html += this.detailRow('classification', 'no diagnostic row for this slot in the payload', PH);
            }
        }
        this.detailsBox.innerHTML = html;
    }

    // Box 2 — statistics for the selected second, computed in Rust
    // (`DiagnosticSecondStats`) so the same numbers can be exported.
    renderStats() {
        if (!this.statsBox || !this.slotGrid) return;
        const { secondStart, second, slots } = this.slotGrid;
        const stats = this.secondStats(second);

        if (!stats) {
            this.statsBox.innerHTML =
                `<div class="stats-group-title">Statistics — second ${secondStart} s (${slots} slots)</div>`
                + '<div class="diag-note">No diagnostic statistics for this second in the payload.</div>';
            return;
        }

        const us = (value) => `${Math.round(value)} µs`;
        const groups = [
            ['Counts', [
                ['samples / nominal pps', `${stats.samples} / ${stats.slots}`],
                ['effective pps of the second', stats.effective_pps.toFixed(2)],
                ['empty slots', `${stats.empty_slots}`],
                ['loss ratio', `${(stats.loss_ratio * 100).toFixed(1)}%`]
            ]],
            ['Δtick intervals', [
                ['mean', us(stats.interval_mean_us)],
                ['median', us(stats.interval_median_us)],
                ['min', us(stats.interval_min_us)],
                ['max', us(stats.interval_max_us)],
                ['std (jitter)', us(stats.interval_std_us)],
                ['p95 vs nominal', `${us(stats.interval_p95_us)} vs ${us(stats.nominal_interval_us)}`]
            ]],
            ['Burst analysis', [
                ['consecutive empty slots (1 / 2 / 3+)',
                    `${stats.burst_histogram[0]} / ${stats.burst_histogram[1]} / ${stats.burst_histogram[2]}`],
                ['longest empty run', `${stats.longest_empty_run}`]
            ]],
            ['Slot offset (lateness)', [
                ['mean', `${stats.offset_mean_us.toFixed(0)} µs`],
                ['min / max', `${stats.offset_min_us} / ${stats.offset_max_us} µs`]
            ]],
            ['Layer counters', [
                ['Δstream − count (dropped at stream)', `${stats.stream_gap_total}`],
                ['Δraw_drop_total', `${stats.drop_total}`],
                ['Δraw_send_backpressure_total', `${stats.backpressure_total}`],
                ['frames with backpressure > 0', `${stats.backpressure_frames}`]
            ]],
            ['Radio quality', [
                ['min / mean / max RSSI',
                    `${orDash(stats.rssi_min_dbm)} / ${stats.rssi_mean_dbm === null ? '—' : stats.rssi_mean_dbm.toFixed(1)} / ${orDash(stats.rssi_max_dbm)} dBm`],
                ['empty slots with low RSSI', `${stats.empty_slots_low_rssi}`]
            ]],
            ['Anomalies', [
                ['records missing FRESH', `${stats.missing_fresh}`],
                ['first-word-invalid count', `${stats.first_word_invalid}`],
                ['channel / phy / width changes', `${stats.config_changes}`]
            ]]
        ];

        let html = `<div class="stats-group-title">Statistics — second ${secondStart} s (${slots} slots`
            + `${stats.partial ? ', partial second' : ''})</div>`;
        for (const [title, rows] of groups) {
            html += `<div class="stats-group-title" style="color: var(--text-secondary)">${title}</div>`;
            for (const [label, value] of rows) {
                html += `<div class="diag-stat"><span class="stat-label">${label}</span>`
                    + `<span class="stat-value">${value}</span></div>`;
            }
        }
        this.statsBox.innerHTML = html;
    }
}

// --- DSP View ---
//
// Every value drawn here comes from the precomputed Rust `dsp` payload block:
//
//   dsp: { interp, target_fs, norm, max_gap_ms, timestamps,
//          null_subcarriers, time_state, panels: [{ id, title, unit,
//          kind, data }] }
//
// The browser never resamples or normalizes: Rust is the single implementation
// of the formulas and this class only formats. Live (streaming) payloads carry
// `dsp: null`, so the toggle is replaced by the "available after streaming is
// closed" note.

// Per-grid-column state, matching the planned `time_state` byte values.
const GRID_STATE = { observed: 0, bridged: 1, masked: 2 };
const GRID_STATE_COLORS = ['#22c55e', '#fbbf24', '#64748b'];

// Maximum visible window per canvas (seconds). This bounds the canvas only —
// the recording and its CSV export are never truncated.
const DSP_MAX_WINDOW_S = 300;

class DspView {
    constructor(visualizer) {
        this.visualizer = visualizer;
        this.section = document.getElementById('dspSection');
        this.toggleBar = document.getElementById('dspToggleBar');
        this.toggle = document.getElementById('dspToggle');
        this.liveNote = document.getElementById('dspLiveNote');
        this.hint = document.getElementById('dspHint');
        this.nullNote = document.getElementById('dspNullNote');
        this.placeholderNote = document.getElementById('dspPlaceholderNote');
        this.hideNullCheckbox = document.getElementById('dspHideNull');
        this.interpSelect = document.getElementById('dspInterpSelect');
        this.normSelect = document.getElementById('dspNormSelect');
        this.gapInput = document.getElementById('dspGapInput');
        this.windowStart = document.getElementById('dspWindowStart');
        this.windowLength = document.getElementById('dspWindowLength');
        this.windowReadout = document.getElementById('dspWindowReadout');
        this.maskCanvas = document.getElementById('dspMaskCanvas');
        this.panels = {

            amplitude: {
                wrapper: document.getElementById('dspAmplitudePanel'),
                canvas: document.getElementById('dspAmplitudeCanvas'),
                tooltip: document.getElementById('dspAmplitudeTooltip'),
                meta: document.getElementById('dspAmplitudeMeta'),
                csv: document.getElementById('dspAmplitudeCsv'),
                lut: 'viridis',
                kind: 'sequential'
            },
            phase: {
                wrapper: document.getElementById('dspPhasePanel'),
                canvas: document.getElementById('dspPhaseCanvas'),
                tooltip: document.getElementById('dspPhaseTooltip'),
                meta: document.getElementById('dspPhaseMeta'),
                csv: document.getElementById('dspPhaseCsv'),
                lut: 'twilight',
                kind: 'cyclic'
            },
            amplitude_norm: {
                wrapper: document.getElementById('dspNormPanel'),
                canvas: document.getElementById('dspNormCanvas'),
                tooltip: document.getElementById('dspNormTooltip'),
                meta: document.getElementById('dspNormMeta'),
                csv: document.getElementById('dspNormCsv'),
                lut: 'rdbu',
                kind: 'diverging'
            }
        };
        this.data = null;
        this.enabled = false;
        this.live = false;
        this.model = null;
        this.offscreen = document.createElement('canvas');
        this.offCtx = this.offscreen.getContext('2d');
        this.margin = { top: 34, right: 118, bottom: 46, left: 62 };
        this.initEvents();
    }

    initEvents() {
        if (this.toggle) {
            this.toggle.addEventListener('change', (e) => this.setEnabled(e.target.checked));
        }
        if (this.hideNullCheckbox) {
            this.hideNullCheckbox.addEventListener('change', () => this.render());
        }
        for (const select of [this.interpSelect, this.normSelect, this.windowLength]) {
            if (select) select.addEventListener('change', () => this.render());
        }
        if (this.gapInput) {
            this.gapInput.addEventListener('input', () => this.render());
        }
        if (this.windowStart) {
            this.windowStart.addEventListener('input', () => this.render());
        }
        for (const [id, panel] of Object.entries(this.panels)) {
            if (panel.canvas) {
                panel.canvas.addEventListener('mousemove', (e) => this.onMouseMove(e, id));
                panel.canvas.addEventListener('mouseleave', () => this.hideTooltip(id));
            }
            if (panel.csv) {
                panel.csv.addEventListener('click', () => this.exportCsv(id));
            }
        }
        window.addEventListener('resize', () => this.render());
    }

    // `live` distinguishes the streaming branch: DSP is never computed there,
    // so the toggle is replaced by the note.
    setData(data, live) {
        this.data = data;
        this.live = !!live;
        const dsp = (data && data.dsp) || null;
        this.dsp = dsp;

        if (this.toggleBar) this.toggleBar.hidden = !dsp;
        if (this.liveNote) this.liveNote.hidden = !(this.live && !dsp);
        if (!dsp) this.setEnabled(false);
        this.syncControls(dsp);
        this.render();
    }

    // The payload fixes the DSP parameters, so the selectors show what was
    // actually computed instead of pretending to re-run it in the browser.
    syncControls(dsp) {
        if (!dsp) return;
        if (this.interpSelect) {
            this.interpSelect.value = dsp.interp || 'cubic_spline';
            this.interpSelect.disabled = true;
        }
        if (this.normSelect) {
            this.normSelect.value = dsp.norm || 'none';
            this.normSelect.disabled = true;
        }
        if (this.gapInput) {
            this.gapInput.value = dsp.max_gap_ms === null || dsp.max_gap_ms === undefined
                ? ''
                : String(dsp.max_gap_ms);
            this.gapInput.disabled = true;
        }
        // The parameters are embedded in an immutable HTML bundle, so explain
        // how to change them rather than leaving dead controls unexplained.
        for (const el of [this.interpSelect, this.normSelect, this.gapInput]) {
            if (el) el.title = 'Fixed when the payload was prepared — re-run visualize with --dsp-interp / --dsp-norm / --dsp-max-gap-ms to change it.';
        }
    }

    setEnabled(on) {
        this.enabled = on;
        if (this.toggle) this.toggle.checked = on;
        if (this.section) this.section.hidden = !on;
        if (!on) {
            for (const id of Object.keys(this.panels)) this.hideTooltip(id);
        }
        this.render();
    }

    // The model is the payload block itself; nothing is derived in the browser.
    currentModel() {
        return this.dsp || null;
    }

    render() {
        if (!this.section) return;
        if (!this.enabled) {
            this.model = null;
            return;
        }

        this.model = this.currentModel();
        if (!this.model) return;

        // Null-subcarrier note, always visible so the masking rule is explicit.
        const hidden = this.hideNullCheckbox && this.hideNullCheckbox.checked;
        if (this.nullNote) {
            const bins = this.model.null_subcarriers;
            this.nullNote.hidden = bins.length === 0;
            this.nullNote.textContent = bins.length === 0
                ? ''
                : `${bins.length} null subcarrier(s) ${hidden ? 'hidden' : 'masked'}: bins ${bins.join(', ')} — `
                    + 'never rendered as 0 and excluded from transforms across the subcarrier axis.';
        }

        // Provenance note: the parameters are baked into the payload, so say
        // what produced these numbers and that the recording is intact.
        if (this.placeholderNote) {
            const masked = this.model.time_state.filter((v) => v === GRID_STATE.masked).length;
            const bridged = this.model.time_state.filter((v) => v === GRID_STATE.bridged).length;
            const resampledFrom = (this.model.interp || 'cubic_spline').replace(/_/g, ' ');
            this.placeholderNote.textContent = 'Computed in Rust when this payload was prepared '
                + `(${resampledFrom} interpolation onto a ${this.model.target_fs.toFixed(0)} Hz grid`
                + `${this.model.norm ? `, normalization: ${this.model.norm.replace(/_/g, ' ')}` : ''}`
                + `${this.model.max_gap_ms === null || this.model.max_gap_ms === undefined ? ', no gap limit' : `, gap limit ${this.model.max_gap_ms} ms`}). `
                + `${bridged} bridged and ${masked} masked grid column(s); masked cells are withheld, not invented. `
                + 'The recording itself is never truncated — CSV exports every grid column.';
        }

        if (this.hint) {
            const meta = this.model;
            const total = this.model.timestamps.length;
            this.hint.textContent = `${meta.interp} · target ${meta.target_fs ? meta.target_fs.toFixed(0) : '—'} Hz · `
                + `${total} grid column(s) · norm: ${meta.norm || 'none'} · `
                + `max gap: ${meta.max_gap_ms === null ? 'no limit' : `${meta.max_gap_ms} ms`}`;
        }

        this.updateWindowControls();
        this.drawMaskStrip();
        for (const panel of this.model.panels) {
            this.drawPanel(panel.id);
        }
        // Hide the normalized panel when it is not part of the model.
        const normPanel = this.panels.amplitude_norm;
        if (normPanel && normPanel.wrapper) {
            normPanel.wrapper.hidden = !this.model.panels.some((p) => p.id === 'amplitude_norm');
        }
    }

    // Window is per canvas and canvas-only: it never truncates the recording.
    windowRange() {
        const total = (this.model && this.model.timestamps && this.model.timestamps.length) || 0;
        const length = this.windowLength ? Number(this.windowLength.value) : 30;
        const duration = total > 0
            ? this.model.timestamps[total - 1] - this.model.timestamps[0]
            : 0;
        const usable = Math.min(length, DSP_MAX_WINDOW_S, Math.max(duration, 0));
        const start = this.windowStart ? Number(this.windowStart.value) : 0;
        const maxStart = Math.max(0, duration - usable);
        return { start: Math.min(start, maxStart), length: usable, duration, maxStart };
    }

    updateWindowControls() {
        if (!this.windowStart || !this.model) return;
        const total = this.model.timestamps.length;
        const duration = total > 0 ? this.model.timestamps[total - 1] - this.model.timestamps[0] : 0;
        const length = this.windowLength ? Number(this.windowLength.value) : 30;
        const maxStart = Math.max(0, duration - Math.min(length, DSP_MAX_WINDOW_S));
        this.windowStart.max = String(maxStart);
        if (Number(this.windowStart.value) > maxStart) this.windowStart.value = String(maxStart);
        if (this.windowReadout) {
            const start = Number(this.windowStart.value);
            this.windowReadout.textContent = `${start.toFixed(1)} – ${(start + Math.min(length, DSP_MAX_WINDOW_S)).toFixed(1)} s`;
        }
    }

    // Grid state strip: one pixel per grid column.
    drawMaskStrip() {
        if (!this.maskCanvas || !this.model) return;
        const states = this.model.time_state;
        const ctx = this.maskCanvas.getContext('2d');
        const dpr = window.devicePixelRatio || 1;
        const rect = this.maskCanvas.getBoundingClientRect();
        const width = Math.max(300, rect.width || 1200);
        const height = 18;
        this.maskCanvas.width = Math.round(width * dpr);
        this.maskCanvas.height = Math.round(height * dpr);
        this.maskCanvas.style.height = `${height}px`;
        ctx.save();
        ctx.scale(dpr, dpr);
        ctx.clearRect(0, 0, width, height);
        const colW = width / Math.max(states.length, 1);
        for (let i = 0; i < states.length; i++) {
            ctx.fillStyle = GRID_STATE_COLORS[states[i]] || GRID_STATE_COLORS[0];
            ctx.fillRect(i * colW, 0, Math.max(colW, 1), height);
        }
        ctx.restore();
    }

    // Rows to draw for a panel: every subcarrier, minus null rows when the
    // checkbox hides them. Physical bin indices are kept for the y axis.
    visibleBins(scCount) {
        const nulls = new Set((this.model && this.model.null_subcarriers) || []);
        const hide = !!(this.hideNullCheckbox && this.hideNullCheckbox.checked);
        const bins = [];
        for (let sc = 0; sc < scCount; sc++) {
            if (hide && nulls.has(sc)) continue;
            bins.push(sc);
        }
        return bins;
    }

    drawPanel(id) {
        const panel = this.panels[id];
        if (!panel || !panel.canvas) return;
        const modelPanel = this.model.panels.find((p) => p.id === id);
        if (!modelPanel) return;

        const canvas = panel.canvas;
        const ctx = canvas.getContext('2d');
        const data = modelPanel.data;
        const scCount = (data[0] && data[0].length) || this.visualizer.getSubcarrierCount();
        const nulls = new Set(this.model.null_subcarriers);
        const hideNull = !!(this.hideNullCheckbox && this.hideNullCheckbox.checked);
        const bins = this.visibleBins(scCount);
        const range = this.windowRange();

        // Columns inside the window.
        const timestamps = this.model.timestamps;
        const total = Math.min(data.length, timestamps.length);
        const cols = [];
        for (let j = 0; j < total; j++) {
            const t = timestamps[j];
            if (t >= range.start - 1e-9 && t <= range.start + range.length + 1e-9) cols.push(j);
        }
        if (cols.length === 0) return;

        // Offscreen: one pixel per (column, visible bin).
        this.offscreen.width = cols.length;
        this.offscreen.height = Math.max(bins.length, 1);
        const img = this.offCtx.createImageData(this.offscreen.width, this.offscreen.height);
        const px = img.data;
        const lut = COLORMAP_LUTS[panel.lut] || COLORMAP_LUTS.viridis;
        const NULL_RGBA = [51, 65, 85];
        const MASKED_RGBA = [71, 85, 105];

        // Value range for this panel. The range is clipped to robust
        // percentiles: a single multipath spike (this capture reaches 29 252
        // against a median of 26) would otherwise pin the scale and collapse
        // 99% of the data into one colour. Values outside are clamped to the
        // end colours, and the true extent is reported in the panel meta.
        let min = Infinity;
        let max = -Infinity;
        const samples = [];
        for (const j of cols) {
            const row = data[j];
            for (const sc of bins) {
                if (nulls.has(sc)) continue;
                const v = row[sc];
                if (!Number.isFinite(v)) continue;
                samples.push(v);
                if (v < min) min = v;
                if (v > max) max = v;
            }
        }
        if (!Number.isFinite(min) || !Number.isFinite(max)) { min = 0; max = 1; }
        const fullMin = min;
        const fullMax = max;
        if (samples.length > 20) {
            samples.sort((a, b) => a - b);
            const lo = samples[Math.floor(0.02 * (samples.length - 1))];
            const hi = samples[Math.floor(0.98 * (samples.length - 1))];
            if (hi > lo) { min = lo; max = hi; }
        }
        // Cyclic panels span −π…π regardless of the data, so the colorbar means
        // something stable and the wrap at ±π is visible.
        if (panel.kind === 'cyclic') { min = -Math.PI; max = Math.PI; }
        if (max - min < 1e-9) max = min + 1e-9;

        for (let ci = 0; ci < cols.length; ci++) {
            const j = cols[ci];
            const state = this.model.time_state[j];
            for (let ri = 0; ri < bins.length; ri++) {
                const sc = bins[ri];
                const py = bins.length - 1 - ri;
                const pIdx = (py * cols.length + ci) * 4;
                if (nulls.has(sc)) {
                    px[pIdx] = NULL_RGBA[0];
                    px[pIdx + 1] = NULL_RGBA[1];
                    px[pIdx + 2] = NULL_RGBA[2];
                    px[pIdx + 3] = 255;
                    continue;
                }
                if (state === GRID_STATE.masked) {
                    px[pIdx] = MASKED_RGBA[0];
                    px[pIdx + 1] = MASKED_RGBA[1];
                    px[pIdx + 2] = MASKED_RGBA[2];
                    px[pIdx + 3] = 255;
                    continue;
                }
                const value = data[j][sc];
                const norm = Math.max(0, Math.min(1, (value - min) / (max - min)));
                const lutIdx = Math.floor(norm * 255) * 3;
                px[pIdx] = lut[lutIdx];
                px[pIdx + 1] = lut[lutIdx + 1];
                px[pIdx + 2] = lut[lutIdx + 2];
                px[pIdx + 3] = 255;
            }
        }
        this.offCtx.putImageData(img, 0, 0);

        // Canvas sizing.
        const rect = canvas.getBoundingClientRect();
        const dpr = window.devicePixelRatio || 1;
        const width = Math.max(600, rect.width || 1200);
        const height = Math.max(200, Math.round(width * 0.22));
        canvas.width = Math.round(width * dpr);
        canvas.height = Math.round(height * dpr);
        canvas.style.height = `${height}px`;

        ctx.save();
        ctx.scale(dpr, dpr);
        ctx.fillStyle = '#0b1120';
        ctx.fillRect(0, 0, width, height);

        const m = this.margin;
        const plotX = m.left;
        const plotY = m.top;
        const plotW = width - m.left - m.right;
        const plotH = height - m.top - m.bottom;

        ctx.save();
        ctx.beginPath();
        ctx.rect(plotX, plotY, plotW, plotH);
        ctx.clip();
        ctx.imageSmoothingEnabled = false;
        ctx.drawImage(this.offscreen, plotX, plotY, plotW, plotH);
        ctx.restore();

        ctx.strokeStyle = '#334155';
        ctx.lineWidth = 1;
        ctx.strokeRect(plotX, plotY, plotW, plotH);

        // Title + meta.
        ctx.fillStyle = '#f8fafc';
        ctx.font = '600 13px -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif';
        ctx.textAlign = 'center';
        ctx.textBaseline = 'bottom';
        ctx.fillText(modelPanel.title, plotX + plotW / 2, plotY - 10);

        // Axes: time (x) and physical subcarrier bins (y).
        ctx.fillStyle = '#94a3b8';
        ctx.font = '10px ui-monospace, SFMono-Regular, Menlo, monospace';
        ctx.textAlign = 'center';
        ctx.textBaseline = 'top';
        const span = Math.max(range.length, 1e-9);
        const step = this.niceStep(span / 6);
        for (let t = 0; t <= span + 1e-9; t += step) {
            const x = plotX + (t / span) * plotW;
            ctx.fillText(`${(range.start + t).toFixed(step < 1 ? 1 : 0)}`, x, plotY + plotH + 8);
        }
        ctx.fillText('Time (s)', plotX + plotW / 2, plotY + plotH + 26);

        ctx.textAlign = 'right';
        ctx.textBaseline = 'middle';
        const yTicks = 5;
        for (let i = 0; i <= yTicks; i++) {
            const sc = Math.round((bins.length - 1) * (i / yTicks));
            const bin = bins[sc] !== undefined ? bins[sc] : 0;
            const y = plotY + plotH - (i / yTicks) * plotH;
            ctx.fillText(String(bin), plotX - 8, y);
        }
        ctx.save();
        ctx.translate(14, plotY + plotH / 2);
        ctx.rotate(-Math.PI / 2);
        ctx.textAlign = 'center';
        ctx.fillText('Subcarrier', 0, 0);
        ctx.restore();

        // Colorbar with its unit.
        const cbX = plotX + plotW + 18;
        const cbW = 14;
        const grad = ctx.createLinearGradient(0, plotY + plotH, 0, plotY);
        for (let i = 0; i <= 10; i++) {
            const norm = i / 10;
            const lutIdx = Math.floor(norm * 255) * 3;
            grad.addColorStop(norm, `rgb(${lut[lutIdx]}, ${lut[lutIdx + 1]}, ${lut[lutIdx + 2]})`);
        }
        ctx.fillStyle = grad;
        ctx.fillRect(cbX, plotY, cbW, plotH);
        ctx.strokeStyle = '#334155';
        ctx.strokeRect(cbX, plotY, cbW, plotH);
        ctx.fillStyle = '#94a3b8';
        ctx.textAlign = 'left';
        ctx.textBaseline = 'middle';
        for (let i = 0; i <= 4; i++) {
            const frac = i / 4;
            const value = min + (max - min) * frac;
            const y = plotY + plotH - frac * plotH;
            ctx.fillText(this.formatTick(value), cbX + cbW + 5, y);
        }
        ctx.save();
        ctx.translate(cbX + cbW + 52, plotY + plotH / 2);
        ctx.rotate(-Math.PI / 2);
        ctx.textAlign = 'center';
        ctx.fillText(modelPanel.unit, 0, 0);
        ctx.restore();

        ctx.restore();

        // Remember the geometry for hover hit-testing.
        panel.plotBounds = { plotX, plotY, plotW, plotH, cols, bins, min, max, panel: modelPanel };
        if (panel.meta) {
            // Say the scale is clipped when it is, so nobody reads the colour
            // bar as the full extent.
            const clipped = fullMax > max + 1e-9 || fullMin < min - 1e-9;
            const scaleText = panel.kind === 'cyclic'
                ? '−π…π'
                : `${min.toFixed(1)}…${max.toFixed(1)}${clipped ? ` (clipped, full ${fullMin.toFixed(1)}…${fullMax.toFixed(1)})` : ''}`;
            panel.meta.textContent = `${cols.length} column(s) shown · ${bins.length}/${scCount} subcarriers · `
                + `${modelPanel.unit} ${scaleText}`;
        }
    }

    // Compact tick label: switches to exponential form when the magnitude makes
    // a fixed-point label unreadable (a 5-digit amplitude, a denormal).
    formatTick(value) {
        if (!Number.isFinite(value)) return String(value);
        const magnitude = Math.abs(value);
        if (magnitude !== 0 && (magnitude >= 10_000 || magnitude < 0.01)) {
            return value.toExponential(1);
        }
        return magnitude >= 100 ? value.toFixed(0) : value.toFixed(1);
    }

    niceStep(raw) {
        const steps = [0.1, 0.2, 0.5, 1, 2, 5, 10, 15, 20, 30, 60, 120, 300];
        for (const s of steps) if (raw <= s) return s;
        return steps[steps.length - 1];
    }

    onMouseMove(event, id) {
        const panel = this.panels[id];
        if (!panel || !panel.plotBounds || !panel.tooltip) return;
        const { plotX, plotY, plotW, plotH, cols, bins, panel: modelPanel } = panel.plotBounds;
        const rect = panel.canvas.getBoundingClientRect();
        const x = event.clientX - rect.left;
        const y = event.clientY - rect.top;
        if (x < plotX || x > plotX + plotW || y < plotY || y > plotY + plotH) {
            this.hideTooltip(id);
            return;
        }
        const ci = Math.min(cols.length - 1, Math.floor(((x - plotX) / plotW) * cols.length));
        const ri = Math.min(bins.length - 1, bins.length - 1 - Math.floor(((y - plotY) / plotH) * bins.length));
        const j = cols[ci];
        const sc = bins[ri];
        const nulls = new Set(this.model.null_subcarriers);
        const state = this.model.time_state[j];
        const stateName = ['observed', 'bridged', 'masked'][state] || 'unknown';
        const value = modelPanel.data[j][sc];
        const t = this.model.timestamps[j];

        let valueText;
        if (nulls.has(sc)) {
            valueText = 'null subcarrier (masked)';
        } else if (state === GRID_STATE.masked) {
            valueText = 'withheld (gap over the limit)';
        } else if (Number.isFinite(value)) {
            valueText = value.toFixed(3);
        } else {
            valueText = '—';
        }
        panel.tooltip.innerHTML = `<div class="tip-title">grid #${j} · SC ${sc} — ${valueText}</div>`
            + `<div class="tip-body">t = ${t.toFixed(3)} s · state: ${stateName} · ${modelPanel.unit}</div>`;
        panel.tooltip.style.display = 'block';
        const wrap = panel.tooltip.parentElement;
        const wrapRect = wrap.getBoundingClientRect();
        let left = event.clientX - wrapRect.left + 14;
        let top = event.clientY - wrapRect.top + 14;
        const tw = panel.tooltip.offsetWidth;
        const th = panel.tooltip.offsetHeight;
        if (left + tw > wrapRect.width - 6) left = event.clientX - wrapRect.left - tw - 10;
        if (top + th > wrapRect.height - 6) top = event.clientY - wrapRect.top - th - 10;
        panel.tooltip.style.left = `${Math.max(4, left)}px`;
        panel.tooltip.style.top = `${Math.max(4, top)}px`;
    }

    hideTooltip(id) {
        const panel = this.panels[id];
        if (panel && panel.tooltip) panel.tooltip.style.display = 'none';
    }

    // CSV export: the FULL recording (never the visible window), one row per
    // grid column, one column per subcarrier, with a '#' provenance header so
    // `pandas.read_csv(path, comment='#')` loads it directly. Null subcarriers
    // follow the "Hide null subcarriers" option, and masked cells are left
    // empty rather than filled with an invented value.
    exportCsv(id) {
        const panel = this.panels[id];
        const modelPanel = this.model && this.model.panels.find((p) => p.id === id);
        if (!panel || !modelPanel) return;

        const data = modelPanel.data;
        const timestamps = this.model.timestamps;
        const rows = Math.min(data.length, timestamps.length);
        const scCount = (data[0] && data[0].length) || 0;
        const nulls = new Set(this.model.null_subcarriers || []);
        const hideNull = !!(this.hideNullCheckbox && this.hideNullCheckbox.checked);
        const bins = hideNull ? this.visibleBins(scCount) : Array.from({ length: scCount }, (_, k) => k);

        const masked = new Set();
        for (let j = 0; j < rows; j++) {
            if (this.model.time_state[j] === GRID_STATE.masked) masked.add(j);
        }

        const lines = [];
        // Provenance header: everything needed to interpret the numbers, so a
        // file kept for later analysis is self-describing.
        lines.push('# RT-Wireless-Sensing — DSP view export');
        lines.push(`# panel=${modelPanel.id} title="${modelPanel.title}" unit=${modelPanel.unit} kind=${modelPanel.kind}`);
        lines.push(`# interp=${this.model.interp} target_fs=${this.model.target_fs} norm=${this.model.norm || 'none'}`);
        lines.push(`# max_gap_ms=${this.model.max_gap_ms === null || this.model.max_gap_ms === undefined ? 'none' : this.model.max_gap_ms}`);
        const src = (this.data && this.data.metadata && this.data.metadata.source) || 'unknown';
        lines.push(`# source="${src}" grid_columns=${rows} subcarriers=${bins.length} null_subcarriers=${hideNull ? 'hidden' : (this.model.null_subcarriers || []).join('|') || 'none'}`);
        lines.push(`# time_state: 0=observed 1=bridged 2=masked; masked_cells=empty`);
        lines.push(`# exported=${new Date().toISOString()}`);

        const header = ['time_s', 'grid_index', 'time_state'].concat(bins.map((k) => `sc_${k}`));
        lines.push(header.join(','));

        const stateNames = ['observed', 'bridged', 'masked'];
        for (let j = 0; j < rows; j++) {
            const row = data[j];
            const cells = new Array(bins.length);
            const isMasked = masked.has(j);
            for (let i = 0; i < bins.length; i++) {
                const value = row[bins[i]];
                cells[i] = isMasked || !Number.isFinite(value) ? '' : String(value);
            }
            const state = stateNames[this.model.time_state[j]] || 'unknown';
            lines.push([timestamps[j].toFixed(6), j, state].concat(cells).join(','));
        }

        const blob = new Blob([lines.join('\n')], { type: 'text/csv;charset=utf-8' });
        const url = URL.createObjectURL(blob);
        const link = document.createElement('a');
        const stem = (src || 'capture').replace(/\.[^.]+$/, '');
        link.href = url;
        link.download = `${stem}_dsp_${modelPanel.id}.csv`;
        document.body.appendChild(link);
        link.click();
        document.body.removeChild(link);
        URL.revokeObjectURL(url);

        if (panel.meta) {
            panel.meta.textContent = `exported ${rows} row(s) × ${bins.length} subcarrier(s) — full recording`;
        }
    }
}

// Global bootstrap
document.addEventListener('DOMContentLoaded', async () => {
    const visualizer = new CsiVisualizer('heatmapCanvas');
    visualizer.diagnosticView = new DiagnosticView(visualizer);
    visualizer.dspView = new DspView(visualizer);
    const loadingOverlay = document.getElementById('loadingOverlay');
    const loadingText = document.getElementById('loadingText');
    let hideTimer = null;

    const hideLoading = () => {
        if (!loadingOverlay || loadingOverlay.style.display === 'none') return;
        clearTimeout(hideTimer);
        loadingOverlay.style.opacity = '0';
        hideTimer = setTimeout(() => {
            loadingOverlay.style.display = 'none';
        }, 300);
    };

    const showWaiting = (elapsedSeconds, warmupSeconds) => {
        if (!loadingOverlay) return;
        clearTimeout(hideTimer);
        loadingOverlay.style.display = 'flex';
        loadingOverlay.style.opacity = '1';
        if (loadingText) {
            const target = Number.isFinite(warmupSeconds) && warmupSeconds > 0 ? warmupSeconds : 3;
            const elapsed = Number.isFinite(elapsedSeconds) ? elapsedSeconds : 0;
            loadingText.textContent = `Waiting for CSI data… ${elapsed.toFixed(1)} s / ${target.toFixed(1)} s`;
        }
    };

    let staticLoaded = false;

    const applyPayload = (payload) => {
        if (!payload) return;

        if (payload.status === 'waiting') {
            showWaiting(payload.elapsed_seconds, payload.warmup_seconds);
            return;
        }

        if (payload.status === 'error') {
            if (loadingText) loadingText.textContent = payload.message || 'Visualization error';
            return;
        }

        // Live server: { status: "ready", data: {...} } — DSP is never computed
        // during streaming, so the DSP view shows its note instead of panels.
        if (payload.status === 'ready' && payload.data && payload.data.amplitudes) {
            visualizer.live = true;
            visualizer.setData(payload.data);
            hideLoading();
            return;
        }

        // Static server: the payload itself is VisualizationData.
        if (payload.amplitudes) {
            visualizer.live = false;
            visualizer.setData(payload);
            hideLoading();
            staticLoaded = true;
        }
    };

    // 1. Check for inlined data (standalone HTML bundle)
    if (window.CSI_DATA && window.CSI_DATA.amplitudes) {
        visualizer.live = false;
        visualizer.setData(window.CSI_DATA);
        hideLoading();
        return;
    }

    // 2. Poll the live/static server every 500 ms.
    let polling = false;
    const poll = async () => {
        if (polling || staticLoaded) return;
        polling = true;
        try {
            const resp = await fetch('/api/data', { cache: 'no-store' });
            if (!resp.ok) throw new Error(`HTTP error ${resp.status}`);
            applyPayload(await resp.json());
        } catch (err) {
            console.warn('Could not fetch /api/data, retrying:', err);
            if (loadingText && loadingOverlay && loadingOverlay.style.display !== 'none') {
                loadingText.textContent = 'Waiting for the visualization server…';
            }
        } finally {
            polling = false;
        }
    };

    await poll();
    setInterval(poll, 500);
});
