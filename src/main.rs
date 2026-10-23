use std::{env, error::Error, path::Path};

mod dsp;
use dsp::{amplitude, fft_peaks, load_csi, load_ticks, mean_std};


fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().collect();
    let dir = Path::new(args.get(1).map(String::as_str).unwrap_or("espectre_data/free_space.npz_FILES"));
    let sc: usize = args.get(2).map_or(Ok(0), |s| s.parse())?;

    let csi = load_csi(&dir.join("csi_data.npy"))?;
    let ticks = load_ticks(&dir.join("device_ticks_us.npy"))?;
    let amp = amplitude(&csi);

    let (n, nsc) = amp.dim();
    assert!(n >= 4 && sc < nsc && ticks.len() == n);

    let mut dt: Vec<f64> = ticks.windows(2).into_iter()
        .map(|w| (w[1] as f64 - w[0] as f64) / 1e6)
        .collect();

    assert!(dt.iter().all(|&v| v > 0.0), "Invalid timestamps");
    dt.sort_by(f64::total_cmp);
    let fs = 1.0 / dt[dt.len() / 2];

    println!("Packets:     {n}");
    println!("Subcarriers: {nsc}");
    println!("Sample rate: {fs:.2} Hz (median interval)");

    let all: Vec<f64> = amp.iter().copied().collect();
    let (mean, std) = mean_std(&all);
    println!("\nGlobal amplitude: mean={mean:.3}, std={std:.3}");

    println!("\n{:<12} {:>12} {:>12}", "Subcarrier", "Mean", "Std");
    for k in 0..nsc {
        let x = amp.column(k).to_vec();
        let (mean, std) = mean_std(&x);
        println!("{k:<12} {mean:>12.3} {std:>12.3}");
    }

    let signal = amp.column(sc).to_vec();
    println!("\nTop FFT bins - SC {sc}:");
    println!("{:<16} {}", "Frequency (Hz)", "Magnitude (a.u.)");

    for (freq, mag) in fft_peaks(&signal, fs) {
        println!("{freq:<16.3} {mag:.5}");
    }

    Ok(())
}
