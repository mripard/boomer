#![warn(missing_debug_implementations)]
// #![warn(missing_docs)]
#![warn(rust_2018_idioms)]
#![deny(clippy::all)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]

use std::{hash::Hasher, time::Instant};

use anyhow::{Context, Result};
use clap::{App, Arg};
use image::{imageops::FilterType, GenericImage, Rgb, Rgba, EncodableLayout};
use nucleid::{
    BufferType, ConnectorStatus, ConnectorUpdate, Device, Format, ObjectUpdate, PlaneType,
    PlaneUpdate,
};
use qrcode::QrCode;
use serde::Serialize;
use simplelog::{ColorChoice, Config, LevelFilter, TermLogger, TerminalMode};
use twox_hash::XxHash64;

const QRCODE_WIDTH: usize = 128;
const QRCODE_HEIGHT: usize = 128;

const HEADER_VERSION_MAJOR: u8 = 2;
const HEADER_VERSION_MINOR: u8 = 0;

const NUM_BUFFERS: u64 = 3;

const PATTERN: &[u8] = include_bytes!("../resources/smpte-color-bars.png");

#[derive(Serialize)]
struct Metadata {
    version: (u8, u8),
    qrcode_width: usize,
    qrcode_height: usize,
    width: usize,
    height: usize,
    hash: u64,
    index: u64,
}

fn main() -> Result<()> {
    let matches = App::new("KMS Crash Test Pattern")
        .arg(
            Arg::with_name("device")
                .short("D")
                .help("DRM Device Path")
                .default_value("/dev/dri/card0"),
        )
        .arg(
            Arg::with_name("debug")
                .long("debug")
                .short("d")
                .help("Enables debug log level"),
        )
        .arg(
            Arg::with_name("trace")
                .long("trace")
                .short("t")
                .conflicts_with("debug")
                .help("Enables trace log level"),
        )
        .get_matches();

    let log_level = if matches.is_present("trace") {
        LevelFilter::Trace
    } else if matches.is_present("debug") {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    };

    TermLogger::init(
        log_level,
        Config::default(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    )
    .context("Couldn't setup our logger.")?;

    let dev_path = matches.value_of("device").unwrap();
    let device = Device::new(dev_path).unwrap();

    let connector = device
        .connectors()
        .into_iter()
        .find(|con| con.status().unwrap_or(ConnectorStatus::Unknown) == ConnectorStatus::Connected)
        .context("No Active Connector")?;

    log::info!("Running from connector {:#?}", connector);

    let mode = connector
        .preferred_mode()
        .context("Couldn't find a mode for the connector")?;

    let width = mode.width();
    let height = mode.height();

    log::info!("Using mode {:#?}", mode);

    let output = device
        .output_from_connector(&connector)
        .context("Couldn't find a valid output for that connector")?;

    let plane = output
        .planes()
        .into_iter()
        .find(|plane| {
            plane.formats().any(|fmt| fmt == Format::RGB888)
                && plane.plane_type() == PlaneType::Overlay
        })
        .context("Couldn't find a plane with the proper format")?;

    let mut img = image::load_from_memory(PATTERN)
        .context("Couldn't load our image")?
        .resize_exact(width as u32, height as u32, FilterType::Nearest);

    for x in 0..QRCODE_WIDTH {
        for y in 0..QRCODE_HEIGHT {
            img.put_pixel(x as u32, y as u32, Rgba([0, 0, 0, 255]));
        }
    }

    let mut img = img.to_rgb8();

    let mut hasher = XxHash64::with_seed(0);
    hasher.write(img.as_bytes());
    let hash = hasher.finish() as u64;

    log::info!("Hash {:#x}", hash);

    let mut buffers: Vec<_> = Vec::new();
    for _idx in 0..NUM_BUFFERS {
        let buffer = device
            .allocate_buffer(BufferType::Dumb, width, height, 24)
            .unwrap()
            .into_framebuffer(Format::RGB888)
            .unwrap();

        buffers.push(buffer);
    }

    log::info!("Setting up the pipeline");

    let first = &buffers[0];
    let mut output = output
        .start_update()
        .set_mode(mode)
        .add_connector(
            ConnectorUpdate::new(&connector)
                .set_property("top margin", 0)
                .set_property("bottom margin", 0)
                .set_property("left margin", 0)
                .set_property("right margin", 0),
        )
        .add_plane(
            PlaneUpdate::new(&plane)
                .set_framebuffer(first)
                .set_source_size(width as f32, height as f32)
                .set_source_coordinates(0.0, 0.0)
                .set_display_size(width, height)
                .set_display_coordinates(0, 0),
        )
        .commit()?;

    log::info!("Starting to output");

    let mut index: u64 = 0;
    loop {
        let frame_start = Instant::now();

        let buffer = &mut buffers[(index % NUM_BUFFERS) as usize];
        let data = buffer.data();

        log::debug!("Switching to frame {}", index);

        let metadata = Metadata {
            version: (HEADER_VERSION_MAJOR, HEADER_VERSION_MINOR),
            qrcode_width: QRCODE_WIDTH,
            qrcode_height: QRCODE_HEIGHT,
            width,
            height,
            hash,
            index,
        };

        let json = serde_json::to_string(&metadata).unwrap();

        log::debug!("Metadata {:#?}", json);

        let qrcode = QrCode::new(json.as_bytes())
            .unwrap()
            .render::<Rgb<u8>>()
            .min_dimensions(QRCODE_WIDTH as u32, QRCODE_HEIGHT as u32)
            .max_dimensions(QRCODE_WIDTH as u32, QRCODE_HEIGHT as u32)
            .build();

        image::imageops::overlay(&mut img, &qrcode, 0, 0);
        data.copy_from_slice(img.as_bytes());

        output = output
            .start_update()
            .add_plane(PlaneUpdate::new(&plane).set_framebuffer(buffer))
            .commit()?;

        index += 1;

        log::debug!(
            "Took {} ms to generate the frame",
            frame_start.elapsed().as_millis()
        );
    }
}
