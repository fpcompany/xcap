use image::RgbaImage;
use std::cell::RefCell;
use std::thread_local;
use xcb::{
    x::{Drawable, GetImage, ImageFormat, ImageOrder, Window},
    Connection,
};

use crate::error::{XCapError, XCapResult};

// Reuse a per-thread XCB connection to avoid reconnect overhead on each capture.
thread_local! {
    static XCB_CONN: RefCell<Connection> = {
        let (conn, _) = Connection::connect(None).expect("Failed to connect to X server");
        RefCell::new(conn)
    };
}

fn get_pixel8_rgba(
    bytes: &[u8],
    x: u32,
    y: u32,
    width: u32,
    bits_per_pixel: u32,
    bit_order: ImageOrder,
) -> (u8, u8, u8, u8) {
    let index = ((y * width + x) * bits_per_pixel / 8) as usize;

    let pixel = if bit_order == ImageOrder::LsbFirst {
        bytes[index]
    } else {
        bytes[index] & 7 << 4 | bytes[index] >> 4
    };

    let r = (pixel >> 6) as f32 / 3.0 * 255.0;
    let g = ((pixel >> 2) & 7) as f32 / 7.0 * 255.0;
    let b = (pixel & 3) as f32 / 3.0 * 255.0;

    (r as u8, g as u8, b as u8, 255)
}

fn get_pixel16_rgba(
    bytes: &[u8],
    x: u32,
    y: u32,
    width: u32,
    bits_per_pixel: u32,
    bit_order: ImageOrder,
) -> (u8, u8, u8, u8) {
    let index = ((y * width + x) * bits_per_pixel / 8) as usize;

    let pixel = if bit_order == ImageOrder::LsbFirst {
        bytes[index] as u16 | (bytes[index + 1] as u16) << 8
    } else {
        (bytes[index] as u16) << 8 | bytes[index + 1] as u16
    };

    let r = (pixel >> 11) as f32 / 31.0 * 255.0;
    let g = ((pixel >> 5) & 63) as f32 / 63.0 * 255.0;
    let b = (pixel & 31) as f32 / 31.0 * 255.0;

    (r as u8, g as u8, b as u8, 255)
}

fn get_pixel24_32_rgba(
    bytes: &[u8],
    x: u32,
    y: u32,
    width: u32,
    bits_per_pixel: u32,
    bit_order: ImageOrder,
) -> (u8, u8, u8, u8) {
    let index = ((y * width + x) * bits_per_pixel / 8) as usize;

    if bit_order == ImageOrder::LsbFirst {
        (bytes[index + 2], bytes[index + 1], bytes[index], 255)
    } else {
        (bytes[index], bytes[index + 1], bytes[index + 2], 255)
    }
}

pub fn xorg_capture(
    window: Window,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> XCapResult<RgbaImage> {
    XCB_CONN.with(|conn_cell| {
        let conn = conn_cell.borrow();

        let setup = conn.get_setup();

        let get_image_cookie = conn.send_request(&GetImage {
            format: ImageFormat::ZPixmap,
            drawable: Drawable::Window(window),
            x: x as i16,
            y: y as i16,
            width: width as u16,
            height: height as u16,
            plane_mask: u32::MAX,
        });

        let get_image_reply = conn
            .wait_for_reply(get_image_cookie)
            .map_err(|e| XCapError::new(format!("xcb wait_for_reply error: {}", e)))?;
        let bytes = get_image_reply.data();
        let depth = get_image_reply.depth();

        let pixmap_format = setup
            .pixmap_formats()
            .iter()
            .find(|item| item.depth() == depth)
            .ok_or(XCapError::new("Not found pixmap format"))?;

        let bits_per_pixel = pixmap_format.bits_per_pixel() as u32;
        let bit_order = setup.bitmap_format_bit_order();

        // Fast-path common depths to reduce per-pixel overhead
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        match depth {
            24 | 32 => {
                // Convert BGR(A) -> RGBA
                let stride = (bits_per_pixel / 8) as usize;
                for row in 0..height as usize {
                    for col in 0..width as usize {
                        let src_index = (row * width as usize + col) * stride;
                        let dst_index = (row * width as usize + col) * 4;
                        if bit_order == ImageOrder::LsbFirst {
                            // bytes: B, G, R, (A?)
                            rgba[dst_index] = bytes[src_index + 2];
                            rgba[dst_index + 1] = bytes[src_index + 1];
                            rgba[dst_index + 2] = bytes[src_index];
                            rgba[dst_index + 3] = 255;
                        } else {
                            // bytes: R, G, B, (A?)
                            rgba[dst_index] = bytes[src_index];
                            rgba[dst_index + 1] = bytes[src_index + 1];
                            rgba[dst_index + 2] = bytes[src_index + 2];
                            rgba[dst_index + 3] = 255;
                        }
                    }
                }
            }
            16 => {
                for yy in 0..height {
                    for xx in 0..width {
                        let index = ((yy * width + xx) * 4) as usize;
                        let (r, g, b, a) = get_pixel16_rgba(bytes, xx, yy, width, bits_per_pixel, bit_order);
                        rgba[index] = r;
                        rgba[index + 1] = g;
                        rgba[index + 2] = b;
                        rgba[index + 3] = a;
                    }
                }
            }
            8 => {
                for yy in 0..height {
                    for xx in 0..width {
                        let index = ((yy * width + xx) * 4) as usize;
                        let (r, g, b, a) = get_pixel8_rgba(bytes, xx, yy, width, bits_per_pixel, bit_order);
                        rgba[index] = r;
                        rgba[index + 1] = g;
                        rgba[index + 2] = b;
                        rgba[index + 3] = a;
                    }
                }
            }
            _ => return Err(XCapError::new(format!("Unsupported {} depth", depth))),
        }

        RgbaImage::from_raw(width, height, rgba)
            .ok_or_else(|| XCapError::new("RgbaImage::from_raw failed"))
    })
}
