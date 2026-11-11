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
    jet: generateColormapLUT(COLORMAPS.jet)
};

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
        this.offscreenCanvas = document.createElement('canvas');
        this.offscreenCtx = this.offscreenCanvas.getContext('2d');

        // Layout Margins (CSS pixels)
        this.margin = {
            top: 55,
            right: 120,
            bottom: 65,
            left: 75
        };

        this.initEvents();
    }

    setData(data) {
        this.data = data;
        this.calculateBounds();
        this.updateMetadataUI();
        this.render();
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
        setElem('metaRate', `${meta.effective_pps.toFixed(2)} PPS`);

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

        this.updateOffscreenBuffer();

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
        const totalDuration = this.data.metadata.duration_seconds || (timestamps.length ? timestamps[timestamps.length - 1] : 0);
        const subcarrierCount = this.data.metadata.subcarriers || 64;

        this.ctx.fillStyle = '#94a3b8';
        this.ctx.font = '11px ui-monospace, SFMono-Regular, Menlo, monospace';

        // X-Axis (Time in seconds)
        this.ctx.textAlign = 'center';
        this.ctx.textBaseline = 'top';

        // Compute sensible time step
        const approxTicks = 8;
        const rawStep = totalDuration / approxTicks;
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

        for (let t = 0; t <= totalDuration + 1e-6; t += step) {
            const frac = totalDuration > 0 ? t / totalDuration : 0;
            if (frac > 1.001) break;
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
            this.ctx.fillText(`${t.toFixed(t < 1 && step < 1 ? 1 : 0)}`, x, py + ph + 8);
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

    initEvents() {
        const tooltip = document.getElementById('heatmapTooltip');

        this.canvas.addEventListener('mousemove', (e) => {
            if (!this.data || !this.plotBounds) return;
            const rect = this.canvas.getBoundingClientRect();
            const mouseX = e.clientX - rect.left;
            const mouseY = e.clientY - rect.top;

            const { x: px, y: py, w: pw, h: ph } = this.plotBounds;

            if (mouseX >= px && mouseX <= px + pw && mouseY >= py && mouseY <= py + ph) {
                const numFrames = this.data.amplitudes.length;
                const numSubcarriers = this.data.metadata.subcarriers || 64;

                const fracX = (mouseX - px) / pw;
                const fracY = (mouseY - py) / ph;

                const frameIdx = Math.min(numFrames - 1, Math.max(0, Math.floor(fracX * numFrames)));
                // Inverted Y: bottom is 0, top is numSubcarriers - 1
                const scIdx = Math.min(numSubcarriers - 1, Math.max(0, Math.floor((1 - fracY) * numSubcarriers)));

                const timeSec = this.data.timestamps && this.data.timestamps[frameIdx] !== undefined
                    ? this.data.timestamps[frameIdx]
                    : (frameIdx / numFrames) * this.data.metadata.duration_seconds;

                const ampVal = this.data.amplitudes[frameIdx][scIdx];

                this.hoverState = { x: mouseX, y: mouseY, frameIdx, scIdx, timeSec, ampVal };

                // Update Readout Bar
                document.getElementById('hoverTime').textContent = `${timeSec.toFixed(3)} s`;
                document.getElementById('hoverSubcarrier').textContent = `${scIdx}`;
                document.getElementById('hoverAmplitude').textContent = `${ampVal.toFixed(2)}`;
                document.getElementById('hoverFrame').textContent = `#${frameIdx}`;

                // Update Floating Tooltip
                if (tooltip) {
                    tooltip.style.display = 'block';
                    tooltip.style.left = `${e.clientX + 15}px`;
                    tooltip.style.top = `${e.clientY + 15}px`;
                    tooltip.innerHTML = `
                        <div><strong>Time:</strong> ${timeSec.toFixed(3)} s (#${frameIdx})</div>
                        <div><strong>Subcarrier:</strong> ${scIdx}</div>
                        <div><strong>Amplitude:</strong> ${ampVal.toFixed(3)} a.u.</div>
                    `;
                }

                this.render();
            } else {
                this.clearHover();
            }
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
                this.render();
            });
        }

        const smoothingToggle = document.getElementById('smoothingToggle');
        if (smoothingToggle) {
            smoothingToggle.addEventListener('change', (e) => {
                this.smoothing = e.target.checked;
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
        const tooltip = document.getElementById('heatmapTooltip');
        if (tooltip) tooltip.style.display = 'none';

        document.getElementById('hoverTime').textContent = '--';
        document.getElementById('hoverSubcarrier').textContent = '--';
        document.getElementById('hoverAmplitude').textContent = '--';
        document.getElementById('hoverFrame').textContent = '--';
        this.render();
    }
}

// Global bootstrap
document.addEventListener('DOMContentLoaded', async () => {
    const visualizer = new CsiVisualizer('heatmapCanvas');
    const loadingOverlay = document.getElementById('loadingOverlay');

    const hideLoading = () => {
        if (loadingOverlay) {
            loadingOverlay.style.opacity = '0';
            setTimeout(() => loadingOverlay.style.display = 'none', 300);
        }
    };

    // 1. Check for inlined data (standalone HTML bundle)
    if (window.CSI_DATA && window.CSI_DATA.amplitudes) {
        visualizer.setData(window.CSI_DATA);
        hideLoading();
        return;
    }

    // 2. Fetch from web server endpoint
    try {
        const resp = await fetch('/api/data');
        if (!resp.ok) throw new Error(`HTTP error ${resp.status}`);
        const data = await resp.json();
        visualizer.setData(data);
    } catch (err) {
        console.warn('Could not fetch /api/data, waiting for input:', err);
        const loadingText = document.getElementById('loadingText');
        if (loadingText) loadingText.textContent = 'No CSI data loaded. Use --serve or export standalone HTML.';
    } finally {
        hideLoading();
    }
});
