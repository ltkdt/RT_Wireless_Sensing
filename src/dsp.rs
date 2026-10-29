use ndarray::{Array1, Array2};
use ndarray_npy::read_npy;
use rustfft::{num_complex::Complex, FftPlanner};
use std::{error::Error, path::Path};


pub fn load_csi(path: &Path) -> Result<Array2<f64>, Box<dyn Error>> { // try to load as i8 first, then f32
    let data = match read_npy::<_, Array2<i8>>(path) {
        Ok(x) => x.mapv(|v| v as f64), // mapv: map value instead of reference
        Err(_) => read_npy::<_, Array2<f32>>(path)?.mapv(f64::from),
    };
    Ok(data)
}

pub fn load_ticks(path: &Path) -> Result<Array1<u64>, Box<dyn Error>> {
    if let Ok(x) = read_npy::<_, Array1<u64>>(path) {
        return Ok(x);
    }
    let x: Array1<i64> = read_npy(path)?;
    Ok(x.mapv(|v| v as u64))
}

pub fn mean_std(x: &[f64]) -> (f64, f64) {
    let mean = x.iter().sum::<f64>() / x.len() as f64; // sum as f64 then divide
    let var = x.iter().map(|v| (v - mean).powi(2) ).sum::<f64>() / x.len() as f64;
    (mean, var.sqrt())
}

pub fn amplitude(csi: &Array2<f64>) -> Array2<f64> {
    let (n, cols) = csi.dim();
    assert!(cols % 2 == 0, "Invalid CSI I/Q format"); // must be even number of columns as each subcarrier has I and Q components

    Array2::from_shape_fn((n, cols / 2), |(i, k)| {
        csi[[i, 2 * k + 1]].hypot(csi[[i, 2 * k]]) // Data format: Q0, I0, Q1, I1, ...
        // hypot: sqrt(I^2 + Q^2)
    })
}

pub fn fft_peaks(signal: &[f64], fs: f64) -> Vec<(f64, f64)> { // This requires more review
    let n = signal.len();
    // Calculate the mean and standard deviation of the signal
    let (mean, _) = mean_std(signal);

    // Remove DC and apply Hann window
    let mut buffer: Vec<Complex<f64>> = signal.iter().enumerate()
        .map(|(i, &v)| {
            let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos();
            Complex::new((v - mean) * w, 0.0)
        })
        .collect();

    FftPlanner::<f64>::new()
        .plan_fft_forward(n)
        .process(&mut buffer); // in-place FFT

    let window_sum = n as f64 / 2.0;

    let mut peaks: Vec<(f64, f64)> = (1..n / 2)
        .map(|k| {
            let freq = k as f64 * fs / n as f64;
            let magnitude = 2.0 * buffer[k].norm() / window_sum;
            (freq, magnitude)
        })
        .filter(|(f, _)| *f >= 0.2 && *f <= 10.0) // filter frequencies between 0.2 Hz and 10 Hz, this is only for testing
        .collect();

    peaks.sort_by(|a, b| b.1.total_cmp(&a.1));
    peaks.truncate(5);
    peaks
}

// use std::{env, error::Error, path::Path};

// mod dsp;
// use dsp::{amplitude, fft_peaks, load_csi, load_ticks, mean_std};


// fn main() -> Result<(), Box<dyn Error>> {
//     let args: Vec<String> = env::args().collect();
//     let dir = Path::new(args.get(1).map(String::as_str).unwrap_or("espectre_data/free_space.npz_FILES"));
//     let sc: usize = args.get(2).map_or(Ok(0), |s| s.parse())?;

//     let csi = load_csi(&dir.join("csi_data.npy"))?;
//     let ticks = load_ticks(&dir.join("device_ticks_us.npy"))?;
//     let amp = amplitude(&csi);

//     let (n, nsc) = amp.dim();
//     assert!(n >= 4 && sc < nsc && ticks.len() == n);

//     let mut dt: Vec<f64> = ticks.windows(2).into_iter()
//         .map(|w| (w[1] as f64 - w[0] as f64) / 1e6)
//         .collect();

//     assert!(dt.iter().all(|&v| v > 0.0), "Invalid timestamps");
//     dt.sort_by(f64::total_cmp);
//     let fs = 1.0 / dt[dt.len() / 2];

//     println!("Packets:     {n}");
//     println!("Subcarriers: {nsc}");
//     println!("Sample rate: {fs:.2} Hz (median interval)");

//     let all: Vec<f64> = amp.iter().copied().collect();
//     let (mean, std) = mean_std(&all);
//     println!("\nGlobal amplitude: mean={mean:.3}, std={std:.3}");

//     println!("\n{:<12} {:>12} {:>12}", "Subcarrier", "Mean", "Std");
//     for k in 0..nsc {
//         let x = amp.column(k).to_vec();
//         let (mean, std) = mean_std(&x);
//         println!("{k:<12} {mean:>12.3} {std:>12.3}");
//     }

//     let signal = amp.column(sc).to_vec();
//     println!("\nTop FFT bins - SC {sc}:");
//     println!("{:<16} {}", "Frequency (Hz)", "Magnitude (a.u.)");

//     for (freq, mag) in fft_peaks(&signal, fs) {
//         println!("{freq:<16.3} {mag:.5}");
//     }

//     Ok(())
// }