# RT-Wireless-sensing

A real-time wireless sensing and localization framework built with Rust, supporting RF signal acquisition, processing, and edge AI inference for applications such as human activity recognition and indoor positioning. Currently this is used for CSI Wifi Sensing

## Structure:
- Rust (inside /src):
 + Source for CSI **real-time** data streaming, basic digital signal processing and AI processing
- Python (inside /scripts):
 + Create plot and perform statistics to validate CSI data.

## Pipeline
The overall pipeline would be:
- Using Espectre (https://github.com/francescopace/espectre) to flash the ESP32 and use to calculate CSI. The project doesn't interfere with this process.
- When CSI data start streaming and the espectre endpoint is opened, this repo will open a port on the current machine and collect CSI data. The CSI data is decoded (currently in the work). The CSI data should be updated every 2 second. 
- The decoded CSI data will be showed on a web interface as a Amplitude heatmap (which can be seen inside plot data), and saved to a npz file at the same time.
- Another project would be training AI model on the collected CSI data and export the model weight as `onnx` file. The current repo would use the model to process the CSI data and give prediction. The intended task right now is using CSI for localization.