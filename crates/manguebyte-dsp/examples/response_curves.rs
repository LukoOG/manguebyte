//! Draws the curves of the processors, as an interface would: frequency
//! responses, transfer curves, an envelope, modulation cycles, echoes and
//! a reverb's decay, each as an SVG picture and a CSV table, to look at.
//!
//! ```text
//! cargo run -p manguebyte-dsp --example response_curves -- <directory>
//! ```
//!
//! Every curve comes from [`manguebyte_dsp::curve`]: a descriptor taken
//! from a processor or made from settings, its [`CurveSpec`] for the axes,
//! and its values along them. The CSV files have one row per point: the
//! series, then `x` and `y` in the units of the axes, which the first line
//! names.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;

use manguebyte_dsp::curve::{
    Axis, Curve, CurveSpec, FrequencyResponse, Point, Scale, Unit, unwrap_phase,
};
use manguebyte_dsp::iir::{Band, Design};
use manguebyte_dsp::{
    Adsr, AdsrParams, Chorus, Compressor, Crossover, CrossoverSlope, Delay, Expander, Filter,
    FilterShape, FirFilter, LadderFilter, LadderMode, Lfo, LfoShape, Limiter, NoiseGate, OnePole,
    OnePoleMode, Phaser, ProcessSpec, Processor, Reverb, ShaperCurve, StateVariableFilter, SvfMode,
    Waveshaper, fir,
};

const RATE: f64 = 48_000.0;
const POINTS: usize = 512;

const SPEC: ProcessSpec = ProcessSpec {
    sample_rate: RATE,
    max_block_size: 512,
    num_channels: 2,
};

/// One line, or set of stems, of a picture.
struct Series {
    label: String,
    points: Vec<Point>,
    stems: bool,
}

/// A picture: axes and the series drawn on them.
struct Plot {
    name: &'static str,
    title: &'static str,
    spec: CurveSpec,
    series: Vec<Series>,
}

impl Plot {
    fn new(name: &'static str, title: &'static str, spec: CurveSpec) -> Self {
        Self {
            name,
            title,
            spec,
            series: Vec::new(),
        }
    }

    /// Adds `curve`, drawn along this plot's horizontal axis.
    fn curve(mut self, label: &str, curve: &dyn Curve) -> Self {
        let mut xs = vec![0.0; POINTS];
        self.spec.x.fill(&mut xs);
        let mut ys = vec![0.0; POINTS];
        curve.fill(&xs, &mut ys);
        self.series.push(Series {
            label: label.to_owned(),
            points: xs
                .iter()
                .zip(&ys)
                .map(|(&x, &y)| Point::new(x, y))
                .collect(),
            stems: false,
        });
        self
    }

    /// Adds the phase of `response`, unwrapped along the frequency axis.
    fn unwrapped_phase(mut self, label: &str, response: &dyn FrequencyResponse) -> Self {
        let mut xs = vec![0.0; POINTS];
        self.spec.x.fill(&mut xs);
        let mut ys: Vec<f64> = xs.iter().map(|&x| response.phase_at(x)).collect();
        unwrap_phase(&mut ys);
        self.series.push(Series {
            label: label.to_owned(),
            points: xs
                .iter()
                .zip(&ys)
                .map(|(&x, &y)| Point::new(x, y))
                .collect(),
            stems: false,
        });
        self
    }

    /// Adds stems.
    fn stems(mut self, label: &str, points: &[Point]) -> Self {
        self.series.push(Series {
            label: label.to_owned(),
            points: points.to_vec(),
            stems: true,
        });
        self
    }

    fn with_y(mut self, min: f64, max: f64) -> Self {
        self.spec.y.min = min;
        self.spec.y.max = max;
        self
    }

    fn write(&self, directory: &Path) -> io::Result<()> {
        fs::write(directory.join(format!("{}.svg", self.name)), self.svg())?;
        fs::write(directory.join(format!("{}.csv", self.name)), self.csv())
    }

    fn csv(&self) -> String {
        let (x, y) = (&self.spec.x, &self.spec.y);
        let mut text = format!(
            "# {}: x in {} ({}), y in {}\nseries,x,y\n",
            self.title,
            unit_name(x.unit),
            scale_name(x.scale),
            unit_name(y.unit)
        );
        for series in &self.series {
            for point in &series.points {
                let _ = writeln!(text, "{},{},{}", series.label, point.x, point.y);
            }
        }
        text
    }

    fn svg(&self) -> String {
        const WIDTH: f64 = 760.0;
        const HEIGHT: f64 = 440.0;
        const LEFT: f64 = 70.0;
        const RIGHT: f64 = 170.0;
        const TOP: f64 = 40.0;
        const BOTTOM: f64 = 50.0;
        const COLOURS: [&str; 8] = [
            "#2a6fdb", "#c92a2a", "#2b8a3e", "#ae3ec9", "#e8590c", "#1098ad", "#343a40", "#a0522d",
        ];
        let (x_axis, y_axis) = (&self.spec.x, &self.spec.y);
        let (plot_w, plot_h) = (WIDTH - LEFT - RIGHT, HEIGHT - TOP - BOTTOM);
        let px = |x: f64| LEFT + plot_w * x_axis.position(x);
        let py = |y: f64| TOP + plot_h * (1.0 - y_axis.position(y));

        let mut svg = String::new();
        let _ = writeln!(
            svg,
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{WIDTH}\" height=\"{HEIGHT}\" \
             font-family=\"sans-serif\" font-size=\"11\">"
        );
        let _ = writeln!(
            svg,
            "<rect width=\"100%\" height=\"100%\" fill=\"white\"/>\
             <text x=\"{LEFT}\" y=\"24\" font-size=\"14\">{}</text>",
            self.title
        );
        let _ = writeln!(
            svg,
            "<clipPath id=\"plot\"><rect x=\"{LEFT}\" y=\"{TOP}\" width=\"{plot_w}\" \
             height=\"{plot_h}\"/></clipPath>"
        );
        for tick in ticks(x_axis) {
            let x = px(tick);
            let _ = writeln!(
                svg,
                "<line x1=\"{x:.1}\" y1=\"{TOP}\" x2=\"{x:.1}\" y2=\"{:.1}\" stroke=\"#e5e5e5\"/>\
                 <text x=\"{x:.1}\" y=\"{:.1}\" text-anchor=\"middle\">{}</text>",
                TOP + plot_h,
                TOP + plot_h + 16.0,
                label(x_axis, tick)
            );
        }
        for tick in ticks(y_axis) {
            let y = py(tick);
            let _ = writeln!(
                svg,
                "<line x1=\"{LEFT}\" y1=\"{y:.1}\" x2=\"{:.1}\" y2=\"{y:.1}\" stroke=\"#e5e5e5\"/>\
                 <text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"end\">{}</text>",
                LEFT + plot_w,
                LEFT - 6.0,
                y + 4.0,
                label(y_axis, tick)
            );
        }
        let _ = writeln!(
            svg,
            "<rect x=\"{LEFT}\" y=\"{TOP}\" width=\"{plot_w}\" height=\"{plot_h}\" fill=\"none\" \
             stroke=\"#888\"/>\
             <text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\">{}</text>\
             <text transform=\"translate(16 {:.1}) rotate(-90)\" text-anchor=\"middle\">{}</text>",
            LEFT + 0.5 * plot_w,
            HEIGHT - 10.0,
            unit_name(x_axis.unit),
            TOP + 0.5 * plot_h,
            unit_name(y_axis.unit)
        );
        for (index, series) in self.series.iter().enumerate() {
            let colour = COLOURS[index % COLOURS.len()];
            let mut path = String::new();
            if series.stems {
                let base = py(y_axis.min.max(0.0).min(y_axis.max));
                for point in &series.points {
                    let (x, y) = (px(point.x), py(point.y));
                    let _ = write!(path, "M{x:.2} {base:.2}L{x:.2} {y:.2}");
                    let _ = write!(
                        svg,
                        "<circle cx=\"{x:.2}\" cy=\"{y:.2}\" r=\"2.5\" fill=\"{colour}\" \
                         clip-path=\"url(#plot)\"/>"
                    );
                }
            } else {
                for (n, point) in series.points.iter().enumerate() {
                    // Far outside the plot, a point is put just outside it
                    // so that the line still leaves at the right place.
                    let y = py(point.y).clamp(TOP - 50.0, TOP + plot_h + 50.0);
                    let command = if n == 0 { 'M' } else { 'L' };
                    let _ = write!(path, "{command}{:.2} {y:.2}", px(point.x));
                }
            }
            let _ = writeln!(
                svg,
                "<path d=\"{path}\" fill=\"none\" stroke=\"{colour}\" stroke-width=\"1.6\" \
                 clip-path=\"url(#plot)\"/>"
            );
            let legend_y = TOP + 14.0 + 16.0 * index as f64;
            let legend_x = LEFT + plot_w + 12.0;
            let _ = writeln!(
                svg,
                "<line x1=\"{legend_x}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"{colour}\" \
                 stroke-width=\"2\"/><text x=\"{:.1}\" y=\"{legend_y:.1}\">{}</text>",
                legend_y - 4.0,
                legend_x + 18.0,
                legend_y - 4.0,
                legend_x + 24.0,
                series.label
            );
        }
        svg.push_str("</svg>\n");
        svg
    }
}

fn unit_name(unit: Unit) -> &'static str {
    match unit {
        Unit::Hertz => "Hz",
        Unit::Decibels => "dB",
        Unit::Radians => "radians",
        Unit::Seconds => "seconds",
        Unit::Milliseconds => "milliseconds",
        Unit::Value => "value",
        Unit::Cycles => "cycles",
    }
}

fn scale_name(scale: Scale) -> &'static str {
    match scale {
        Scale::Linear => "linear",
        Scale::Logarithmic => "logarithmic",
    }
}

/// Where the grid lines of an axis go: 1, 2 and 5 of every decade along a
/// logarithmic axis, half turns along a phase axis, and round steps along
/// any other.
fn ticks(axis: &Axis) -> Vec<f64> {
    let (low, high) = (axis.min.min(axis.max), axis.min.max(axis.max));
    if axis.scale == Scale::Logarithmic {
        let mut ticks = Vec::new();
        let mut decade = 10f64.powf(low.log10().floor());
        while decade <= high {
            for step in [1.0, 2.0, 5.0] {
                let tick = decade * step;
                if tick >= low * 0.999 && tick <= high * 1.001 {
                    ticks.push(tick);
                }
            }
            decade *= 10.0;
        }
        return ticks;
    }
    let step = if axis.unit == Unit::Radians {
        core::f64::consts::FRAC_PI_2
    } else {
        let rough = (high - low) / 6.0;
        let magnitude = 10f64.powf(rough.log10().floor());
        [1.0, 2.0, 5.0, 10.0]
            .into_iter()
            .map(|m| m * magnitude)
            .find(|&step| step >= rough)
            .unwrap_or(rough)
    };
    let first = (low / step).ceil() as i64;
    let last = (high / step).floor() as i64;
    (first..=last).map(|i| i as f64 * step).collect()
}

fn label(axis: &Axis, value: f64) -> String {
    match axis.unit {
        Unit::Hertz if value >= 1_000.0 => format!("{}k", value / 1_000.0),
        Unit::Radians => format!("{:.0}π/2", value / core::f64::consts::FRAC_PI_2),
        _ => {
            let rounded = (value * 1e6).round() / 1e6;
            format!("{rounded}")
        }
    }
}

fn main() -> io::Result<()> {
    let Some(directory) = std::env::args().nth(1) else {
        eprintln!("usage: response_curves <directory>");
        std::process::exit(2);
    };
    let directory = Path::new(&directory);
    fs::create_dir_all(directory)?;
    for plot in plots() {
        plot.write(directory)?;
        println!("{}", directory.join(format!("{}.svg", plot.name)).display());
    }
    Ok(())
}

fn plots() -> Vec<Plot> {
    let mut plots = Vec::new();

    // An equaliser: four bands, and the curve of all four.
    let mut bands = [
        Filter::<f32>::new(FilterShape::LowShelf { gain_db: 4.0 }, 120.0, 0.707),
        Filter::<f32>::new(FilterShape::Peak { gain_db: -6.0 }, 450.0, 1.8),
        Filter::<f32>::new(FilterShape::Peak { gain_db: 3.0 }, 3_000.0, 0.9),
        Filter::<f32>::new(FilterShape::HighShelf { gain_db: -5.0 }, 9_000.0, 0.707),
    ];
    for band in &mut bands {
        band.prepare(&SPEC);
    }
    let responses = bands.each_ref().map(Filter::response);
    let equaliser = responses.magnitude();
    let mut plot = Plot::new(
        "equaliser",
        "Equaliser: four bands and their sum",
        equaliser.spec(),
    );
    for (response, name) in responses
        .iter()
        .zip(["low shelf", "peak", "peak", "high shelf"])
    {
        plot = plot.curve(name, &response.magnitude());
    }
    plots.push(plot.curve("all four", &equaliser));
    let phase = responses.phase();
    plots.push(
        Plot::new(
            "equaliser_phase",
            "Equaliser: phase, unwrapped",
            phase.spec(),
        )
        .unwrapped_phase("all four", &responses)
        .with_y(-1.5, 1.5),
    );

    // Second-order filters at 1 kHz.
    let shapes = [
        ("low-pass", FilterShape::Lowpass),
        ("high-pass", FilterShape::Highpass),
        ("band-pass", FilterShape::Bandpass),
        ("notch", FilterShape::Notch),
    ];
    let mut plot = None;
    for (name, shape) in shapes {
        let mut filter = Filter::<f64>::new(shape, 1_000.0, 2.0);
        filter.prepare(&SPEC);
        let curve = filter.response();
        let current = plot.take().unwrap_or_else(|| {
            Plot::new("biquad", "Biquad at 1 kHz, Q 2", curve.magnitude().spec())
                .with_y(-48.0, 12.0)
        });
        plot = Some(current.curve(name, &curve.magnitude()));
    }
    plots.extend(plot);

    // The state-variable filter, at rising resonance.
    let mut plot = None;
    for q in [0.5, 0.707, 2.0, 8.0] {
        let mut filter = StateVariableFilter::<f32>::new(SvfMode::Lowpass, 2_000.0, q);
        filter.prepare(&SPEC);
        let response = filter.response();
        let curve = response.magnitude();
        let current = plot.take().unwrap_or_else(|| {
            Plot::new("svf", "State-variable low-pass at 2 kHz", curve.spec()).with_y(-60.0, 24.0)
        });
        plot = Some(current.curve(&format!("Q {q}"), &curve));
    }
    plots.extend(plot);

    // The one-pole.
    let mut plot = None;
    for (name, mode) in [
        ("low-pass", OnePoleMode::Lowpass),
        ("high-pass", OnePoleMode::Highpass),
    ] {
        let mut filter = OnePole::<f32>::new(mode, 500.0);
        filter.prepare(&SPEC);
        let response = filter.response();
        let curve = response.magnitude();
        let current = plot
            .take()
            .unwrap_or_else(|| Plot::new("one_pole", "One-pole at 500 Hz", curve.spec()));
        plot = Some(current.curve(name, &curve));
    }
    plots.extend(plot);

    // The ladder, every mode at the same resonance.
    let mut plot = None;
    for mode in LadderMode::ALL {
        let mut filter = LadderFilter::<f32>::new(mode, 1_000.0).with_resonance(0.7);
        filter.prepare(&SPEC);
        let response = filter.response();
        let curve = response.magnitude();
        let current = plot.take().unwrap_or_else(|| {
            Plot::new(
                "ladder",
                "Ladder at 1 kHz, resonance 0.7 (linear model)",
                curve.spec(),
            )
            .with_y(-60.0, 12.0)
        });
        plot = Some(current.curve(&format!("{mode:?}"), &curve));
    }
    plots.extend(plot);

    // High-order designs.
    let cutoff = Band::Lowpass { cutoff_hz: 2_000.0 };
    let designs = [
        ("Butterworth 8", Design::butterworth(cutoff, 8)),
        ("Chebyshev I 6, 1 dB", Design::chebyshev_i(cutoff, 6, 1.0)),
        (
            "Chebyshev II 6, 60 dB",
            Design::chebyshev_ii(Band::Lowpass { cutoff_hz: 3_000.0 }, 6, 60.0),
        ),
        ("elliptic 6", Design::elliptic(cutoff, 6, 0.5, 70.0)),
    ];
    let mut plot = None;
    for (name, design) in designs {
        let curve = design.response(RATE);
        let current = plot.take().unwrap_or_else(|| {
            Plot::new("iir", "IIR low-pass designs", curve.magnitude().spec()).with_y(-100.0, 6.0)
        });
        plot = Some(current.curve(name, &curve.magnitude()));
    }
    plots.extend(plot);

    // A crossover's bands and their sum.
    let mut crossover = Crossover::<f32>::new(CrossoverSlope::Lr4, &[200.0, 2_000.0]);
    crossover.prepare(&SPEC);
    let sum = crossover.response();
    let mut plot = Plot::new(
        "crossover",
        "Linkwitz-Riley crossover, 24 dB per octave",
        sum.magnitude().spec(),
    )
    .with_y(-48.0, 6.0);
    for band in 0..crossover.num_bands() {
        let response = crossover.band_response(band).unwrap();
        plot = plot.curve(&format!("band {band}"), &response.magnitude());
    }
    plots.push(plot.curve("sum", &sum.magnitude()));

    // A FIR low-pass.
    let taps = fir::lowpass(fir::kaiser_taps(80.0, 0.05), 0.2, 80.0);
    let fir = FirFilter::<f32>::new(&taps).expect("taps");
    let curve = fir.response(RATE);
    plots.push(
        Plot::new(
            "fir",
            "FIR low-pass, Kaiser window, 80 dB",
            curve.magnitude().spec(),
        )
        .curve(&format!("{} taps", fir.len()), &curve.magnitude()),
    );

    // Dynamics.
    let compressor = Compressor::<f32>::new()
        .with_threshold_db(-24.0)
        .with_ratio(4.0);
    let soft = Compressor::<f32>::new()
        .with_threshold_db(-24.0)
        .with_ratio(4.0)
        .with_knee_db(12.0)
        .with_makeup_db(6.0);
    let expander = Expander::<f32>::new()
        .with_threshold_db(-40.0)
        .with_ratio(3.0)
        .with_range_db(30.0);
    let gate = NoiseGate::<f32>::new()
        .with_threshold_db(-50.0)
        .with_floor_db(-30.0);
    let limiter = Limiter::<f32>::new().with_ceiling_db(-3.0);
    let spec = expander.transfer().spec();
    plots.push(
        Plot::new("dynamics", "Dynamics: steady level out against in", spec)
            .curve("compressor 4:1", &compressor.transfer())
            .curve("soft knee, makeup", &soft.transfer())
            .curve("expander 3:1", &expander.transfer())
            .curve("gate, floor -30", &gate.transfer())
            .curve("limiter -3 dB", &limiter.transfer()),
    );

    // Shapers.
    let mut plot = Plot::new(
        "shaper",
        "Waveshapers, 6 dB of drive",
        ShaperCurve::Tanh.spec(),
    );
    for curve in [
        ShaperCurve::HardClip,
        ShaperCurve::Tanh,
        ShaperCurve::SoftClip,
        ShaperCurve::Fold,
    ] {
        let shaper = Waveshaper::<f32>::new(curve).with_drive_db(6.0);
        plot = plot.curve(&format!("{curve:?}"), &shaper);
    }
    plots.push(plot);

    // An envelope, held for half a second.
    let adsr = Adsr::new(AdsrParams {
        attack: 0.05,
        decay: 0.2,
        sustain: 0.6,
        release: 0.4,
    });
    let shape = adsr.shape(0.5);
    plots.push(
        Plot::new("envelope", "ADSR held for 0.5 s", shape.spec())
            .curve("level", &shape)
            .stems("corners", shape.points()),
    );

    // Modulation.
    let mut plot = None;
    for shape in [
        LfoShape::Sine,
        LfoShape::Triangle,
        LfoShape::SawUp,
        LfoShape::Square,
        LfoShape::SampleAndHold,
    ] {
        let cycle = Lfo::new(shape, 1.0).cycle();
        let current = plot
            .take()
            .unwrap_or_else(|| Plot::new("lfo", "LFO shapes, one cycle", cycle.spec()));
        plot = Some(current.curve(&format!("{shape:?}"), &cycle));
    }
    plots.extend(plot.map(|plot| plot.with_y(-1.2, 1.2)));

    let mut chorus = Chorus::<f32>::new().with_voices(3).with_spread(1.0);
    chorus.prepare(&SPEC);
    let first = chorus.sweep(0, 0).unwrap();
    let mut plot = Plot::new(
        "chorus",
        "Chorus: delay of each voice over a cycle",
        first.spec(),
    );
    for voice in 0..chorus.voices() {
        let sweep = chorus.sweep(voice, 0).unwrap();
        plot = plot.curve(&format!("voice {voice}"), &sweep);
    }
    let mut flanger = Chorus::<f32>::flanger().with_depth_ms(2.5);
    flanger.prepare(&SPEC);
    let sweep = flanger.sweep(0, 0).unwrap();
    plots.push(plot);
    plots.push(
        Plot::new("flanger", "Flanger: delay over a cycle", sweep.spec())
            .curve("left", &sweep)
            .curve("right", &flanger.sweep(0, 1).unwrap()),
    );

    let mut phaser = Phaser::<f32>::new().with_spread(0.5);
    phaser.prepare(&SPEC);
    let sweep = phaser.sweep(0);
    plots.push(
        Plot::new(
            "phaser",
            "Phaser: stage frequency over a cycle",
            sweep.spec(),
        )
        .curve("left", &sweep)
        .curve("right", &phaser.sweep(1)),
    );

    // Echoes.
    let mut delay = Delay::<f32>::new(1.0)
        .with_time(0.125)
        .with_feedback(0.6)
        .with_mix(0.5);
    delay.prepare(&SPEC);
    let taps = delay.taps();
    let mut points = vec![Point::default(); 64];
    let count = taps.fill(&mut points);
    let spec = taps.spec();
    plots.push(
        Plot::new("delay", "Delay: 125 ms, feedback 0.6, mix 0.5", spec)
            .stems("taps", &points[..count]),
    );

    // A reverb's decay.
    let mut plot = None;
    for damping in [0.0, 0.3, 0.6, 1.0] {
        let mut reverb = Reverb::<f32>::new()
            .with_decay_seconds(2.5)
            .with_damping(damping);
        reverb.prepare(&SPEC);
        let decay = reverb.decay();
        let current = plot
            .take()
            .unwrap_or_else(|| Plot::new("reverb_rt60", "Reverb: RT60, decay 2.5 s", decay.spec()));
        plot = Some(current.curve(&format!("damping {damping}"), &decay));
    }
    plots.extend(plot);
    let mut reverb = Reverb::<f32>::new()
        .with_decay_seconds(2.5)
        .with_damping(0.6);
    reverb.prepare(&SPEC);
    let decay = reverb.decay();
    let low = decay.energy_at(125.0);
    let mut plot = Plot::new(
        "reverb_energy",
        "Reverb: level of the tail, damping 0.6",
        low.spec(),
    );
    for frequency in [125.0, 1_000.0, 4_000.0, 12_000.0] {
        plot = plot.curve(&format!("{frequency} Hz"), &decay.energy_at(frequency));
    }
    plots.push(plot);

    plots
}
