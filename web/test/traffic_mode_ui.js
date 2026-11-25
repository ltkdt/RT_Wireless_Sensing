// Headless check of the traffic-mode web changes, against the *shipped*
// web/visualizer.js.
//
// There is no JS test harness in this repo, and the payload/hint logic is worth
// checking for real rather than by eye: it decides what the Packet Rate card and
// the diagnostic legend say. This loads the actual file, stubs just enough DOM
// to call the metadata/diagnostic methods, and asserts the PLAN.md §4 tables.
//
//   node web/test/traffic_mode_ui.js
//
// Not part of `cargo test`; it exists so those tables are verified rather than
// assumed. It found a real crash while being written: `DiagnosticView`'s
// overview tooltip called `getTrafficMode()` on itself, but that accessor lives
// on `CsiVisualizer`, so hovering the legend row threw. Keep this in step with
// the Rust metadata: `metadata.traffic_mode` and `metadata.nominal_pps_source`.

const fs = require('fs');
const path = require('path');
const vm = require('vm');

const source = fs.readFileSync(path.join(__dirname, '..', 'visualizer.js'), 'utf8');

// --- Minimal DOM stub ------------------------------------------------------
// Only what updateMetadataUI/renderOverview touch: element lookups returning
// objects that record textContent/classList/title.
function makeDom() {
    const elements = new Map();
    const make = (id) => {
        const classes = new Set();
        return {
            id,
            textContent: '',
            title: '',
            innerHTML: '',
            hidden: false,
            style: {},
            offsetWidth: 0,
            classList: {
                toggle: (name, on) => { if (on) classes.add(name); else classes.delete(name); },
                add: (name) => classes.add(name),
                remove: (name) => classes.delete(name),
                contains: (name) => classes.has(name),
            },
            getBoundingClientRect: () => ({ left: 0, top: 0, bottom: 0, right: 0, width: 0, height: 0 }),
        };
    };
    return {
        elements,
        getElementById: (id) => {
            if (!elements.has(id)) elements.set(id, make(id));
            return elements.get(id);
        },
        querySelector: () => null,
        addEventListener: () => {},
        documentElement: { style: { setProperty: () => {} } },
    };
}

function loadScript() {
    const document = makeDom();
    const sandbox = {
        console,
        document,
        window: {},
        // The file's trailing `init` runs on import; stub what it needs so the
        // module-level definitions still evaluate.
        fetch: () => Promise.reject(new Error('no network in this harness')),
        setInterval: () => 0,
        requestAnimationFrame: () => 0,
        devicePixelRatio: 1,
        Image: function () {},
        innerWidth: 1280,
        innerHeight: 800,
    };
    sandbox.window.document = document;
    sandbox.globalThis = sandbox;
    vm.createContext(sandbox);
    // `class` and `const` at top level are lexical bindings inside the script,
    // not properties of the context object, so export the ones under test.
    const epilogue = `
      globalThis.__exports = { CsiVisualizer, DiagnosticView, causeLabel, causeLayer,
        CAUSE_LABELS, CAUSE_LAYERS, CAUSE_LABELS_INTERNAL, CAUSE_LAYERS_INTERNAL,
        OVERVIEW_TIPS, OVERVIEW_TIPS_INTERNAL };
    `;
    vm.runInContext(source + '\n' + epilogue, sandbox, { filename: 'visualizer.js' });
    Object.assign(sandbox, sandbox.__exports);
    return { sandbox, document };
}

// --- Assertions -----------------------------------------------------------
let failures = 0;
function check(label, actual, expected) {
    const ok = actual === expected;
    if (!ok) failures++;
    console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${ok ? '' : `\n       expected: ${expected}\n       actual:   ${actual}`}`);
}
function checkContains(label, haystack, needle) {
    const ok = typeof haystack === 'string' && haystack.includes(needle);
    if (!ok) failures++;
    console.log(`${ok ? 'ok  ' : 'FAIL'} ${label}${ok ? '' : `\n       expected to contain: ${needle}\n       actual: ${haystack}`}`);
}

const { sandbox, document } = loadScript();
const CsiVisualizer = sandbox.CsiVisualizer;
if (!CsiVisualizer) {
    console.error('FAIL could not load CsiVisualizer from web/visualizer.js');
    process.exit(1);
}

// Build a visualizer without running the browser constructor (which binds
// canvases), then inject a payload: the methods under test only read
// `this.data` and the stub DOM.
function withPayload(metadata) {
    const v = Object.create(CsiVisualizer.prototype);
    v.data = { metadata };
    v.overlayLabel = null;
    return v;
}

const baseMeta = {
    start_time_formatted: '0.000 s',
    duration_seconds: 5.0,
    total_frames: 501,
    subcarriers: 64,
    effective_pps: 100.01,
};

console.log('\n--- Packet Rate card: the §4.3 hint table ---');
// The hint is a pure function of (traffic_mode, nominal_pps_source). Note the
// Rust invariant asserted further below: `nominal_pps == Some` always implies
// `nominal_pps_source != null`, so the `source: null` rows are reached only by
// a hand-edited payload or a pre-change one — and there the fallback wording is
// today's, which is what we want.
const cases = [
    // label,                       mode,        source,      nominal, expected hint
    ['internal + device', 'internal', 'device', 100, 'Device target PPS vs measured rate'],
    ['internal + argument', 'internal', 'argument', 50, '--pps fallback (device rate unavailable)'],
    ['external + argument', 'external', 'argument', 20, 'Requested --pps vs measured rate'],
    ['external + no rate at all', 'external', null, null, 'No --pps argument — default 20 PPS'],
    ['legacy (no mode) + argument', null, 'argument', 20, 'Requested --pps vs measured rate'],
    ['legacy (no mode) + no rate', null, null, null, 'No --pps argument — default 20 PPS'],
];

for (const [label, mode, source, nominalPps, expectedHint] of cases) {
    const v = withPayload({ ...baseMeta, traffic_mode: mode, nominal_pps_source: source, nominal_pps: nominalPps });
    const hint = v.nominalHint(v.getTrafficMode(), v.getNominalSource(), v.getNominalPps(), v.getRequestedPps() !== null);
    check(`${label}: hint`, hint, expectedHint);
}

console.log('\n--- Traffic row label ---');
for (const [mode, expected] of [['internal', 'Internal'], ['external', 'External'], [null, '—'], [undefined, '—']]) {
    const v = withPayload({ ...baseMeta, traffic_mode: mode });
    v.updateMetadataUI();
    check(`traffic_mode=${mode}: Traffic row`, document.getElementById('metaTrafficMode').textContent, expected);
}

console.log('\n--- Slot grid rate (getNominalPps) ---');
{
    // The device rate must drive the grid, even though it is not "requested".
    const v = withPayload({ ...baseMeta, traffic_mode: 'internal', nominal_pps_source: 'device', nominal_pps: 100 });
    check('internal device nominal drives the grid', v.getNominalPps(), 100);
    check('  ...but is not reported as a --pps argument', v.getRequestedPps(), null);

    const ext = withPayload({ ...baseMeta, traffic_mode: 'external', nominal_pps_source: 'argument', nominal_pps: 20 });
    check('external argument drives the grid', ext.getNominalPps(), 20);
    check('  ...and is reported as requested', ext.getRequestedPps(), 20);

    // Offline, no provenance: the historical default, unchanged.
    const legacy = withPayload({ ...baseMeta });
    check('no nominal at all falls back to DEFAULT_NOMINAL_PPS', legacy.getNominalPps(), 20);

    // Opinionated: a float nominal must still yield an integer slot count.
    const frac = withPayload({ ...baseMeta, nominal_pps_source: 'device', nominal_pps: 99.6 });
    check('fractional nominal rounds to whole slots', frac.getNominalPps(), 100);
}

console.log('\n--- D14: generator_missed relabel ---');
{
    const v = withPayload({ ...baseMeta });
    const ext = 'no record generated — generator (desktop) or firmware';
    const int = 'no record generated — device generator or firmware';

    check('external: label unchanged', sandbox.causeLabel('generator_missed', 'external'), ext);
    check('null (legacy): label unchanged', sandbox.causeLabel('generator_missed', null), ext);
    check('internal: relabelled', sandbox.causeLabel('generator_missed', 'internal'), int);

    check('external: layer stays generator', sandbox.causeLayer('generator_missed', 'external'), 'generator');
    check('null: layer stays generator', sandbox.causeLayer('generator_missed', null), 'generator');
    check('internal: layer becomes rx', sandbox.causeLayer('generator_missed', 'internal'), 'rx');

    // Every other cause must be mode-blind.
    for (const cause of ['device_drop', 'desktop_backpressure', 'air_loss', 'ambiguous']) {
        check(`${cause} label is mode-blind`, sandbox.causeLabel(cause, 'internal'), sandbox.causeLabel(cause, 'external'));
        check(`${cause} layer is mode-blind`, sandbox.causeLayer(cause, 'internal'), sandbox.causeLayer(cause, 'external'));
    }

    // The bucket must never disappear: the legend order is unchanged and still
    // lists five causes in both modes.
    const shared = sandbox.CAUSE_LABELS;
    check('five causes are labelled', Object.keys(shared).length, 5);
    check('internal override touches only generator_missed',
        Object.keys(sandbox.CAUSE_LABELS_INTERNAL).join(','), 'generator_missed');

    // The tip follows the same rule.
    // The tooltip lives on DiagnosticView, which is where the panel is.
    const tipView = Object.create(sandbox.DiagnosticView.prototype);
    tipView.visualizer = withPayload({ ...baseMeta, traffic_mode: 'external' });
    tipView.data = tipView.visualizer.data;
    check('external tip mentions the marker', tipView.overviewTip('generator_missed'), sandbox.OVERVIEW_TIPS.generator_missed);
    tipView.visualizer.data.metadata.traffic_mode = 'internal';
    const intTip = tipView.overviewTip('generator_missed');
    check('internal tip blames the device', intTip, sandbox.OVERVIEW_TIPS_INTERNAL.generator_missed);
    checkContains('internal tip text', intTip, 'device generator');
}

console.log('\n--- Legacy payload renders as before ---');
{
    // A pre-change payload: no traffic_mode, no nominal_pps_source at all.
    const v = withPayload({ ...baseMeta, nominal_pps: 20 });
    v.updateMetadataUI();
    check('legacy: Traffic row is —', document.getElementById('metaTrafficMode').textContent, '—');
    check('legacy: hint is today\'s text', document.getElementById('metaNominalRateHint').textContent, 'Requested --pps vs measured rate');
    check('legacy: nominal shown', document.getElementById('metaNominalRate').textContent, '20 PPS');
    check('legacy: effective unchanged', document.getElementById('metaRate').textContent, '100.01 PPS');
    check('legacy: Traffic row muted', document.getElementById('metaTrafficMode').classList.contains('metric-pair-default'), true);
}

console.log('\n--- Muted styling only when the rate is a guess ---');
{
    const device = withPayload({ ...baseMeta, traffic_mode: 'internal', nominal_pps_source: 'device', nominal_pps: 100 });
    device.updateMetadataUI();
    check('device-sourced nominal is not italicised',
        document.getElementById('metaNominalRate').classList.contains('metric-pair-default'), false);

    const none = withPayload({ ...baseMeta, traffic_mode: 'external', nominal_pps_source: null });
    none.updateMetadataUI();
    check('no declared rate is italicised',
        document.getElementById('metaNominalRate').classList.contains('metric-pair-default'), true);
}

console.log('\n--- Payload invariant: nominal and source travel together ---');
{
    // `CollectionConfig::resolve_nominal` returns (None, None) or
    // (Some(_), Some(_)) — it never emits a rate without its provenance, so the
    // "rate present, source null" combination only arises in hand-edited JSON.
    // Treating an absent source as "requested" is then the right fallback: the
    // number really did come from somewhere, and claiming otherwise would be
    // worse than attributing it to the argument.
    const v = withPayload({ ...baseMeta, nominal_pps: 20, nominal_pps_source: null });
    check('absent source + present rate => treated as requested', v.getRequestedPps(), 20);
    check('  ...so the hint claims a --pps argument', v.nominalHint(v.getTrafficMode(), v.getNominalSource(), v.getNominalPps(), true), 'Requested --pps vs measured rate');

    // And the genuinely rate-less payload says the opposite.
    const bare = withPayload({ ...baseMeta, nominal_pps: null, nominal_pps_source: null });
    check('no rate + no source => nothing is requested', bare.getRequestedPps(), null);
    check('  ...and the hint says so',
        bare.nominalHint(bare.getTrafficMode(), bare.getNominalSource(), bare.getNominalPps(), bare.getRequestedPps() !== null),
        'No --pps argument — default 20 PPS');
}

console.log('\n--- Capture overview: filled slots, not records ---');
{
    // The overview headline is `filled_slots / total_slots`, with the records
    // that landed in an already-filled slot reported separately. Records and
    // slots are different things, so this is checked against the payload the
    // Rust side actually emits rather than by eye.
    const { DiagnosticView } = sandbox;
    const view = Object.create(DiagnosticView.prototype);
    view.enabled = true;
    view.data = {
        diagnostics: {
            summary: {
                total_slots: 500,
                filled_slots: 454,
                filled_records: 501,
                extra_records: 47,
                empty_slots: 46,
                loss_ratio: 46 / 500,
            },
        },
    };
    view.diagnosticData = () => view.data.diagnostics;
    view.records = () => (view.data.diagnostics.records || []);
    view.trafficMode = () => 'internal';
    view.overviewKpis = document.getElementById('overviewKpis');
    view.overviewLegend = document.getElementById('overviewLegend');
    view.overviewBox = document.getElementById('diagOverviewBox');
    view.overviewCanvas = null;
    view.renderOverview();

    const html = view.overviewKpis.innerHTML;
    checkContains('KPI headline is filled_slots / total_slots', html, '454 / 500');
    checkContains('extra records are a separate line', html, 'extra records');
    checkContains('the record count is not the headline', html.includes('>501 / ') ? 'yes' : 'no', 'no');
}

console.log('\n--- Legacy payload without summary: derive filled_slots ---');
{
    // A payload written before `filled_slots`/`extra_records` existed must
    // still render: the web layer counts distinct (second, slot) pairs itself.
    const { DiagnosticView } = sandbox;
    const view = Object.create(DiagnosticView.prototype);
    view.enabled = true;
    view.data = {
        diagnostics: {
            records: [
                { second: 0, slot: 0 },
                { second: 0, slot: 0 },
                { second: 0, slot: 1 },
            ],
            empty_slots: [{ second: 0, slot: 2, cause: 'generator_missed' }],
        },
    };
    view.diagnosticData = () => view.data.diagnostics;
    view.overviewKpis = document.getElementById('overviewKpis');
    view.overviewLegend = document.getElementById('overviewLegend');
    view.overviewBox = document.getElementById('diagOverviewBox');
    view.overviewCanvas = null;
    view.renderOverview();

    const html = view.overviewKpis.innerHTML;
    checkContains('legacy payload counts 2 distinct slots of 4', html, '2 / 4');
}

console.log(failures === 0 ? '\nALL CHECKS PASSED' : `\n${failures} CHECK(S) FAILED`);
process.exit(failures === 0 ? 0 : 1);
