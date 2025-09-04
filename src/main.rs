use clap::{Parser, Subcommand, ValueEnum};
use log::info;
use std::time::Duration;
use std::{thread, time};

mod protocol;

use protocol::{send_magic, CVITekHeader, Param1, CRC, IMG_ALIGN};

// two minutes should be plenty
const POLL_TIMEOUT: Duration = Duration::from_secs(120);
const POLL_PERIOD: Duration = Duration::from_millis(50);
const TEN_SECS: Duration = Duration::from_secs(10);
const HALF_SEC: Duration = Duration::from_millis(500);

const KILO: usize = 1024;
const MEGA: usize = 1024 * 1024;
// Transfer rate is about 167 KB/s based on real measurements.
const RATE: usize = 167;

const USB_VID_CVITEK: u16 = 0x3346;
const USB_PID_USB_COM: u16 = 0x1000;

#[allow(non_camel_case_types)]
#[derive(ValueEnum, Copy, Clone, Debug, PartialEq, Eq)]
enum Board {
    MilkV_DuoS,
}

impl std::fmt::Display for Board {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.to_possible_value()
            .expect("no values are skipped")
            .get_name()
            .fmt(f)
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    Info,
    /// Write file to SRAM and execute
    #[clap(verbatim_doc_comment)]
    Run {
        file_name: String,
    },
    /// Load file to RAM, address defined by code running on the SoC
    #[clap(verbatim_doc_comment)]
    Load {
        file_name: String,
    },
}

/// Sophgo/CVITek mask ROM loader tool
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Command to run
    #[command(subcommand)]
    cmd: Command,
}

fn print_port_info(info: &serialport::UsbPortInfo) {
    let mf = info.manufacturer.as_ref().map_or("", String::as_str);
    let pi = info.product.as_ref().map_or("", String::as_str);
    let sn = info.serial_number.as_ref().map_or("", String::as_str);
    info!("{mf} {pi} {:04x}:{:04x} ({sn})", info.vid, info.pid);
}

fn poll_dev() -> String {
    let now = time::Instant::now();

    while time::Instant::now() <= now + POLL_TIMEOUT {
        if let Ok(ports) = serialport::available_ports() {
            for p in ports {
                let name = p.port_name;
                if let serialport::SerialPortType::UsbPort(info) = p.port_type {
                    if info.vid == USB_VID_CVITEK && info.pid == USB_PID_USB_COM {
                        print_port_info(&info);
                        return name;
                    }
                }
            }
        }
        thread::sleep(POLL_PERIOD);
    }
    panic!("timeout waiting for CVITek USB device");
}

fn connect() -> std::boxed::Box<dyn serialport::SerialPort> {
    let dev = poll_dev();

    // We managed to identify the device so far. Let's poll another round for opening it.
    let open_timeout = time::Instant::now() + POLL_TIMEOUT;
    while time::Instant::now() <= open_timeout {
        if let Ok(d) = serialport::new(dev.clone(), 115_200)
            .timeout(TEN_SECS)
            .open()
        {
            return d;
        }

        std::thread::sleep(POLL_PERIOD);
    }

    panic!("Failed to open serial port {dev}");
}

fn align_payload(payload: &mut Vec<u8>) {
    let sz = payload.len();
    let aligned = if sz % IMG_ALIGN == 0 {
        sz
    } else {
        (sz / IMG_ALIGN + 1) * IMG_ALIGN
    };
    payload.truncate(aligned);
    println!("ℹ️ Size: {sz}; aligned: {aligned}");

    if aligned > MEGA {
        let kb = aligned / KILO;
        let s = kb / RATE;
        println!("Expect this to take about {s}s ({RATE}KB/s).");
    }
}

fn send_payload(payload: &[u8]) {
    let now = time::SystemTime::now();
    println!("⏳ Waiting for CVITek USB devices...");
    let mut port = connect();
    send_magic(&mut port);

    println!("➡️ Send file...");
    protocol::send_payload(&mut port, &payload);

    println!("🎉 Done. ");

    match now.elapsed() {
        Ok(elapsed) => println!("This took {} seconds.", elapsed.as_secs()),
        // the system clock went backwards!
        _ => {}
    }
}

fn main() {
    let cmd = Cli::parse().cmd;
    env_logger::init();

    match cmd {
        Command::Load { file_name } => {
            let mut payload = std::fs::read(file_name).unwrap();
            align_payload(&mut payload);
            send_payload(&payload);
        }
        Command::Run { file_name } => {
            let mut payload = std::fs::read(file_name).unwrap();
            align_payload(&mut payload);

            let checksum = CRC.checksum(&payload);
            info!("Checksum: {checksum:04x}");

            let checksum = checksum.to_le_bytes();
            let param1 = Param1 {
                bl2_img_size: (payload.len() as u32).to_le_bytes(),
                bl2_img_cksum: [checksum[0], checksum[1], 0xfe, 0xca],
                ..Default::default()
            };

            let checksum = param1.checksum();
            info!("Header checksum: {checksum:04x}");

            let checksum = checksum.to_le_bytes();
            let header = CVITekHeader {
                param1_checksum: [checksum[0], checksum[1], 0xfe, 0xca],
                param1,
                ..Default::default()
            };
            info!("Header {header:02x?}");

            println!("⏳ Waiting for CVITek USB devices...");
            let mut port = connect();
            send_magic(&mut port);
            std::thread::sleep(HALF_SEC);

            println!("➡️ Send header...");
            let mut port = connect();
            protocol::send_payload(&mut port, header.to_slice());
            std::thread::sleep(HALF_SEC);

            send_payload(&payload);
        }
        Command::Info => {
            println!("nothing to see here :)");
        }
    }
}
