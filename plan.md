# Data Visualization Module

## 1. Description

The Data Visualization module provides a graphical interface for visualizing decoded Wi-Fi CSI data. It retrieves CSI records from the existing Decoder module and displays them as a 2D amplitude heatmap.

The visualization should resemble a scientific heatmap generated using MATLAB or Matplotlib, with time on the X-axis, subcarrier index on the Y-axis, and amplitude represented by color intensity.

The module must be implemented primarily in Rust, with native HTML, CSS, and Vanilla JavaScript used exclusively for the web-based visualization component.

## 2. Key Features

### 2.1. CSI Amplitude Heatmap

Display CSI amplitude data as a 2D heatmap using the HTML Canvas 2D API.

**Heatmap configuration:**
- **X-axis:** Measurement time (seconds), representing the temporal progression of CSI measurements.
- **Y-axis:** Subcarrier index, ranging from 0 to 63 (64 subcarriers).
- **Color intensity:** CSI amplitude value, where different colors represent different amplitude levels.
- **Colorbar:** Display a color scale indicating the correspondence between colors and amplitude values.

**Data representation:**

Each decoded CSI record contains 64 amplitude values, corresponding to 64 subcarriers at a specific measurement timestamp.

Multiple records should be combined into a 2D matrix:

`Amplitude Matrix: [Number of Time Frames, 64]`

The heatmap must arrange these records along the time axis to visualize amplitude variations across both time and subcarriers.

**Visualization requirements:**
- Use a scientific colormap such as Viridis, Plasma, or Jet.
- Display axis labels and numerical tick marks.
- Include a chart title: "CSI Amplitude Heatmap".
- Support different measurement durations and data sizes.
- Preserve the correct temporal ordering of CSI records.

### 2.2. Measurement Information

Display relevant measurement information alongside the heatmap:
- Measurement start time.
- Measurement duration.
- Total number of decoded CSI frames.

### 2.3. Classification Results

Provide a dedicated area to display the predicted activity or gesture class when classification results are available.

This module is responsible only for visualization and must not implement classification or model inference.

## 3. Required Technologies and Libraries

| Technology | Purpose |
|---|---|
| Rust | Main application logic and integration with the Decoder module |
| HTML | Dashboard structure |
| CSS | Layout and styling |
f| HTML Canvas 2D API | Heatmap rendering |

Do not introduce large frontend frameworks such as React, Vue, or Angular.

Avoid additional charting libraries unless absolutely necessary. Implement the heatmap rendering directly using Canvas 2D.

## 4. Architecture and Design Considerations

- Keep the visualization module independent of the existing CSI Decoder implementation.
- Reuse the existing Decoder API to retrieve decoded CSI records.
- Do not modify existing CSI acquisition, decoding, or processing modules.
- Keep Rust as the primary implementation language.
- Use HTML, CSS, and Vanilla JavaScript only for the web visualization layer.
- Organize the visualization components so they can be integrated into a future Rust-based web server.
- Separate data preparation from heatmap rendering to facilitate future extensions.

## 5. General Development Guidelines

1. Read `README.md` and the existing `docs/` directory before implementation to understand the project architecture and available APIs.
2. Inspect the existing source code to identify how the Decoder module exposes decoded CSI records.
3. Do not duplicate existing decoding functionality.
4. Do not modify existing CSI modules. If modifications are necessary, stop and explain what needs to be changed before proceeding.
5. Keep the implementation minimal. Avoid introducing unnecessary dependencies or abstractions.
6. Ensure that the visualization module can run independently for initial testing and can be integrated into a future web server.

## 6. Initial Experiment

Use the existing Decoder module to decode `capture_v8.bin` and visualize the resulting CSI data.

**Expected decoded data:**
- 1 timestamp per CSI frame.
- 64 amplitude values per frame.
- 64 phase values per frame.

For this experiment, only use:
- Measurement timestamp.
- 64 CSI amplitude values.

Phase data is not required.

**Expected output:**

A 2D CSI amplitude heatmap resembling the provided reference image, showing how CSI amplitude changes over time across all 64 subcarriers.

The initial implementation should focus exclusively on correct data decoding, temporal alignment, and heatmap visualization. Do not implement real-time streaming or additional signal-processing features at this stage.