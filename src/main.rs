#![warn(rust_2018_idioms)]
#![deny(clippy::all)]
#![deny(clippy::pedantic)]
#![deny(clippy::nursery)]
#![deny(clippy::cargo)]
#![allow(clippy::unreadable_literal)]

use std::hash::Hasher;
use anyhow::{Context, Result};
use byteorder::ByteOrder;
use byteorder::LittleEndian;
use clap::App;
use clap::Arg;
use image::imageops::FilterType;
use nucleid::{BufferType, ConnectorStatus, ConnectorUpdate, Device, Format, ObjectUpdate, PlaneType, PlaneUpdate};
use twox_hash::XxHash32;

const HEADER_VERSION_MAJOR: u8 = 0x42;
const HEADER_VERSION_MINOR: u8 = 0x14;

const NUM_BUFFERS: usize = 3;

fn main() -> Result<()> {
    let matches = App::new("KMS Crash Test Pattern")
        .arg(Arg::with_name("device")
                .short("D")
                .help("DRM Device Path")
                .default_value("/dev/dri/card0"))
        .arg(Arg::with_name("image")
             .required(true))
        .get_matches();

    let img_path = matches.value_of("image").unwrap();
    let dev_path = matches.value_of("device").unwrap();
    let device = Device::new(dev_path).unwrap();

    let connector = device
        .connectors()
        .into_iter()
        .find(|con| {
            con.status().unwrap_or(ConnectorStatus::Unknown) == ConnectorStatus::Connected
        })
        .context("No Active Connector")?;

    println!(
        "Running from connector {}-{}",
        connector.connector_type(),
        connector.connector_type_id()
    );

    // let mode = connector
    //     .preferred_mode()
    //     .context("Couldn't find a mode for the connector")?;

    let mode = connector.modes()
        .context("Couldn't retrieve the connector modes")?
        .into_iter()
        // .find(|mode| mode.width() == 640 && mode.height() == 480 && mode.refresh() == 60)
        .find(|mode| mode.width() == 1280 && mode.height() == 720 && mode.refresh() == 60)
        .context("Couldn't find our mode")?;

    let width = mode.width();
    let height = mode.height();

    println!(
        "Using mode {}x{}@{}",
        mode.width(),
        mode.height(),
        mode.refresh()
    );

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

    let img = image::open(img_path).unwrap()
                                   .resize_exact(width as u32,
                                                 height as u32,
                                                 FilterType::Nearest);
    let img_data = img.to_bgra8().into_vec();

    println!("Opened image {}", img_path);

    let mut hasher = XxHash32::with_seed(0);
    hasher.write(&img_data[10..]);
    let hash = hasher.finish() as u32;

    println!("Hash {:#x}", hash);

    let mut buffers: Vec<_> = Vec::new();
    for _idx in 0..NUM_BUFFERS {
        let mut buffer = device
            .allocate_buffer(BufferType::Dumb, width, height, 24)
            .unwrap()
            .into_framebuffer(Format::RGB888)
            .unwrap();

        let data = buffer.data();
        // data.copy_from_slice(&img_data);

        data[0] = HEADER_VERSION_MAJOR;
        data[1] = HEADER_VERSION_MINOR;
        data[2] = 0;
        LittleEndian::write_u16(&mut data[3..5], 0);
        LittleEndian::write_u16(&mut data[8..10], hash as u16);
        LittleEndian::write_u16(&mut data[12..14], (hash >> 16) as u16);

        let mut hasher = XxHash32::with_seed(0);
        hasher.write(&data[15..]);
        let hash = hasher.finish() as u32;

        println!("Hash {:#x}", hash);

        LittleEndian::write_u16(&mut data[6..8], hash as u16);
        LittleEndian::write_u16(&mut data[9..11], (hash >> 16) as u16);

        buffers.push(buffer);
    }

    println!("Setting up the pipeline");

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
                .set_framebuffer(&first)
                .set_source_size(width as f32, height as f32)
                .set_source_coordinates(0.0, 0.0)
                .set_display_size(width, height)
                .set_display_coordinates(0, 0),
        )
        .commit()?;

    println!("Starting to output");

    let mut index = 0x0000;
    loop {
        let buffer = &mut buffers[index % NUM_BUFFERS];
        let data = buffer.data();

        LittleEndian::write_u16(&mut data[3..5], index as u16);

        output = output
            .start_update()
            .add_plane(PlaneUpdate::new(&plane)
                .set_framebuffer(&buffer)
            )
            .commit()?;

        index = index + 1;
    }
}