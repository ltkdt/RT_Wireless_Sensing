/*
 * Open the ESPectre CSI HTTP stream and save its raw binary data.
 *
 * - GET /csi opens an HTTP response stream containing CSI data in V8 format.
 * - bytes_stream() reads the response body incrementally, one chunk at a time,
 *   avoiding the need to buffer the entire stream in memory.
 * - Each chunk is written directly to capture_v8.bin in the current working
 *   directory. If the file already exists, it is overwritten.
 * - `total` tracks the cumulative number of bytes written to the file.
 * - Stream chunks are not guaranteed to align with CSI record boundaries.
 *   A dedicated V8 parser is required to decode individual CSI records.
 * - The file contains raw binary data; no CSI decoding, DSP, or AI inference
 *   is performed here.
 * - Ensure ESPectre sensing is running and the traffic generator is configured
 *   appropriately before opening the stream.
 *
 * References:
 * https://github.com/francescopace/espectre/blob/main/docs/API.md?utm_source=chatgpt.com#csi-collection
 * https://github.com/francescopace/espectre/blob/main/docs/API.md?utm_source=chatgpt.com
 * https://github.com/francescopace/espectre/blob/main/docs/CSI.md?utm_source=chatgpt.com
 */


use std::{error::Error, net::SocketAddr, sync::Arc};
use futures_util::StreamExt;
use reqwest::Client;
use tokio::{
    io::AsyncWriteExt,
    net::UdpSocket,
    time::{interval, sleep, Duration},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let ip : &str = "10.178.22.9"; // ESP32 IP 
    let base : String = format!("http://{ip}:62587/espectre/v1"); // Espectre endpoint 
    let origin: &str = "https://test.espectre.dev";
    let client : Client = Client::new(); // HTTP Client provided by reqwest
    let pps : u64 = 20;
    let packet_duration = Duration::from_millis(1000 / pps);

    // 1. Check CSI capability
    let caps = client
        .get(format!("{base}/capabilities"))
        .header("Origin", origin)
        .send().await?
        .error_for_status()?
        .text().await?;
    println!("Capabilities: {caps}");

    // 2. Enable external traffic mode
    let result = client
        .patch(format!("{base}/sensing"))       // PATCH request to enable external traffic mode
        .header("Origin", origin)
        .json(&serde_json::json!({
            "traffic_generator_mode": "external"
        }))
        .send().await?
        .error_for_status()?
        .text().await?;
    println!("Sensing: {result}");

    // 3. Send UDP markers at 20 PPS
    // bind to a random port on all interfaces, and send UDP packets to the ESP32 at 20 PPS
    let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?); // Arc: multiple threads can share the same socket
    let sender = socket.clone(); // In the future, there can be more task that use the same socket. Currently, we only have one task that sends UDP packets.
    let target: SocketAddr = format!("{ip}:5555").parse()?;

    tokio::spawn(async move {
        let marker = [0xF0, 0x9F, 0x91, 0xBB]; // Espectre 
        let mut timer = interval(packet_duration);

        loop {
            timer.tick().await; // `PPS` markers per second
            if let Err(e) = sender.send_to(&marker, target).await {
                eprintln!("UDP error: {e}");
                break;
            }
        }
    });

    sleep(Duration::from_millis(200)).await; // Wait for the ESP32 to start sending CSI packets

    // 4. Open the raw CSI stream
    let response = client
        .get(format!("{base}/csi"))
        .header("Origin", origin)
        .send().await?
        .error_for_status()?;

    let mut stream = response.bytes_stream();
    let mut file = tokio::fs::File::create("capture_v8.bin").await?; // Create a file to store the raw binary file 
    let mut total = 0u64;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        total += chunk.len() as u64;
        println!("Received {total} bytes");
    }

    Ok(())
}
