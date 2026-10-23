
import os
import argparse
import numpy as np
import matplotlib
from scipy.signal import stft, detrend

matplotlib.use("Agg")
import matplotlib.pyplot as plt

# Interpolation function
def interp(x, time_uniform, time):
    return np.column_stack([
        np.interp(time_uniform, time, x[:, k])
        for k in range(x.shape[1])
    ])

# Resample CSI data to uniform time intervals
def resample_csi(time, data):
    fs = 1 / np.median(np.diff(time))
    t_uniform = np.arange(time[0], time[-1], 1 / fs)

    resampled = interp(data.real, t_uniform, time)

    if np.iscomplexobj(data):
        resampled = resampled + 1j * interp(data.imag, t_uniform, time)

    return t_uniform, resampled, fs

def load_data(data_dir):
    csi = np.load(os.path.join(data_dir, "csi_data.npy"))
    ticks = np.load(os.path.join(data_dir, "device_ticks_us.npy"))
    rssi = np.load(os.path.join(data_dir, "rssi_dbm.npy"))

    # CSI format: [Q0, I0, Q1, I1, ...]
    Q = csi[:, 0::2].astype(np.float64)
    I = csi[:, 1::2].astype(np.float64)

    H = I + 1j * Q
    time = (ticks.astype(np.float64) - float(ticks[0])) / 1e6

    assert np.all(np.diff(time) > 0), "Timestamps must be strictly increasing"

    return time, np.abs(H), np.angle(H), rssi


def save_figure(fig, label, plot_type, output_dir):
    filepath = os.path.join(output_dir, f"{label}_{plot_type}.png")
    fig.tight_layout()
    fig.savefig(filepath, dpi=300, bbox_inches="tight")
    plt.close(fig)
    print(f"Saved: {filepath}")


def plot_amplitude(label, output_dir, time, amplitude):
    fig, ax = plt.subplots(figsize=(12, 4))
    selected_sc = np.linspace(0, amplitude.shape[1] - 1, 6, dtype=int)

    for k in selected_sc:
        ax.plot(time, amplitude[:, k], label=f"SC {k}", linewidth=1)

    ax.set(
        xlabel="Time (s)",
        ylabel="CSI Amplitude (a.u.)",
        title=f"{label} - CSI Amplitude vs. Time"
    )
    ax.legend(ncol=3)
    ax.grid(alpha=0.3)

    save_figure(fig, label, "csi_amplitude", output_dir)


def plot_amplitude_heatmap(label, output_dir, time, amplitude):
    fig, ax = plt.subplots(figsize=(12, 5))

    im = ax.pcolormesh(
        time,
        np.arange(amplitude.shape[1]),
        amplitude.T,
        shading="auto",
        cmap="viridis"
    )

    ax.set(
        xlabel="Time (s)",
        ylabel="Subcarrier Index",
        title=f"{label} - CSI Amplitude Heatmap"
    )
    fig.colorbar(im, ax=ax, label="Amplitude (a.u.)")

    save_figure(fig, label, "csi_amplitude_heatmap", output_dir)


def plot_phase(label, output_dir, time, phase):
    fig, ax = plt.subplots(figsize=(12, 4))
    selected_sc = np.linspace(0, phase.shape[1] - 1, 6, dtype=int)

    for k in selected_sc:
        ax.plot(time, phase[:, k], label=f"SC {k}", linewidth=0.8)

    ax.set(
        xlabel="Time (s)",
        ylabel="Phase (rad)",
        title=f"{label} - Raw CSI Phase vs. Time"
    )
    ax.legend(ncol=3)
    ax.grid(alpha=0.3)

    save_figure(fig, label, "csi_phase", output_dir)


def plot_packet_interval(label, output_dir, time):
    dt = np.diff(time)
    fig, ax = plt.subplots(figsize=(12, 4))

    ax.plot(time[1:], dt * 1000, linewidth=0.8)
    ax.axhline(
        50, linestyle="--", color="red",
        label="20 PPS target (50 ms)"
    )

    ax.set(
        xlabel="Time (s)",
        ylabel="Inter-packet Interval (ms)",
        title=f"{label} - CSI Packet Timing"
    )
    ax.legend()
    ax.grid(alpha=0.3)

    save_figure(fig, label, "packet_interval", output_dir)


def plot_rssi(label, output_dir, time, rssi):
    fig, ax = plt.subplots(figsize=(12, 4))

    ax.plot(time, rssi, linewidth=1)
    ax.set(
        xlabel="Time (s)",
        ylabel="RSSI (dBm)",
        title=f"{label} - RSSI vs. Time"
    )
    ax.grid(alpha=0.3)

    save_figure(fig, label, "rssi", output_dir)

def plot_amplitude_fft(label, data_dir, output_dir, time, amplitude):
    _, amp, fs = resample_csi(time, amplitude)

    amp = detrend(amp, axis=0)
    spectrum = np.abs(np.fft.rfft(amp, axis=0))
    freqs = np.fft.rfftfreq(len(amp), d=1 / fs)

    fig, ax = plt.subplots(figsize=(12, 5))

    im = ax.pcolormesh(
        freqs,
        np.arange(amp.shape[1]),
        20 * np.log10(spectrum.T + 1e-12),
        shading="auto",
        cmap="viridis"
    )

    ax.set(
        xlabel="Temporal Frequency (Hz)",
        ylabel="Subcarrier Index",
        title=f"{label} - CSI Amplitude FFT"
    )
    fig.colorbar(im, ax=ax, label="Magnitude (dB, relative)")

    save_figure(fig, label, "csi_amplitude_fft", output_dir)


def plot_amplitude_stft(label, data_dir, output_dir, time, amplitude):
    t, amp, fs = resample_csi(time, amplitude)
    amp = detrend(amp, axis=0)

    nperseg = min(128, len(t))
    f, ts, Z = stft(
        amp,
        fs=fs,
        window="hann",
        nperseg=nperseg,
        noverlap=nperseg // 2,
        axis=0
    )

    power = np.mean(np.abs(Z)**2, axis=1)

    fig, ax = plt.subplots(figsize=(12, 5))
    im = ax.pcolormesh(
        ts + t[0], f,
        10 * np.log10(power + 1e-12),
        shading="auto",
        cmap="viridis"
    )

    ax.set(
        xlabel="Time (s)",
        ylabel="Temporal Frequency (Hz)",
        title=f"{label} - CSI Amplitude Spectrogram"
    )
    fig.colorbar(im, ax=ax, label="Power (dB, relative)")

    save_figure(fig, label, "csi_amplitude_stft", output_dir)


def main():
    parser = argparse.ArgumentParser(description="Inspect Espectre CSI Data")
    parser.add_argument("--data_dir", required=True, help="Espectre data folder")
    parser.add_argument("--output_dir", default="plots", help="Output folder")
    parser.add_argument("--label", required=True, help="Gesture/case label")
    args = parser.parse_args()

    os.makedirs(args.output_dir, exist_ok=True)

    time, amplitude, phase, rssi = load_data(args.data_dir)

    dt = np.diff(time)
    observed_pps = (len(time) - 1) / (time[-1] - time[0])

    print(f"Packets: {len(time)}")
    print(f"Subcarriers: {amplitude.shape[1]}")
    print(f"Duration: {time[-1]:.2f} s")
    print(f"Observed PPS: {observed_pps:.2f}")
    print(f"Median interval: {np.median(dt)*1000:.2f} ms")

    plot_amplitude(args.label, args.output_dir, time, amplitude)
    plot_amplitude_heatmap(args.label, args.output_dir, time, amplitude)
    plot_phase(args.label, args.output_dir, time, phase)
    plot_packet_interval(args.label, args.output_dir, time)
    plot_rssi(args.label, args.output_dir, time, rssi)
    plot_amplitude_fft(args.label, args.data_dir, args.output_dir, time, amplitude)
    plot_amplitude_stft(args.label, args.data_dir, args.output_dir, time, amplitude)

    print("\nAll figures saved successfully!")


if __name__ == "__main__":
    main()
