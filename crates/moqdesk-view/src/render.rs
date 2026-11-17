//! Scaling the remote framebuffer into the window and compositing the cursor.

use crate::decode::Image;

#[derive(Clone)]
pub struct CursorShape {
	pub width: usize,
	pub height: usize,
	pub hot_x: i32,
	pub hot_y: i32,
	pub rgba: Vec<u8>,
}

/// Maps between window pixels and remote framebuffer pixels (aspect-preserving fit).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
	pub scale: f64,
	pub offset_x: f64,
	pub offset_y: f64,
	pub remote: (usize, usize),
}

impl Viewport {
	pub fn fit(window: (usize, usize), remote: (usize, usize)) -> Self {
		let scale = (window.0 as f64 / remote.0 as f64).min(window.1 as f64 / remote.1 as f64);
		let offset_x = (window.0 as f64 - remote.0 as f64 * scale) / 2.0;
		let offset_y = (window.1 as f64 - remote.1 as f64 * scale) / 2.0;
		Self {
			scale,
			offset_x,
			offset_y,
			remote,
		}
	}

	pub fn remote_point(self, x: f64, y: f64) -> (u16, u16) {
		let rx = ((x - self.offset_x) / self.scale).clamp(0.0, (self.remote.0 - 1) as f64);
		let ry = ((y - self.offset_y) / self.scale).clamp(0.0, (self.remote.1 - 1) as f64);
		(rx as u16, ry as u16)
	}
}

/// Nearest-neighbour scale `image` into `out` (window-sized, 0RGB), letterboxed in black.
pub fn blit(image: &Image, out: &mut [u32], window: (usize, usize)) -> Viewport {
	let (ww, wh) = window;
	let vp = Viewport::fit(window, (image.width, image.height));
	out.fill(0);
	let x0 = vp.offset_x.round().max(0.0) as usize;
	let y0 = vp.offset_y.round().max(0.0) as usize;
	let dw = ((image.width as f64 * vp.scale).round() as usize).min(ww - x0);
	let dh = ((image.height as f64 * vp.scale).round() as usize).min(wh - y0);
	if (dw, dh) == (image.width, image.height) {
		for y in 0..dh {
			let src = &image.pixels[y * image.width..][..dw];
			out[(y0 + y) * ww + x0..][..dw].copy_from_slice(src);
		}
		return vp;
	}
	let xs: Vec<usize> = (0..dw)
		.map(|x| ((x as f64 / vp.scale) as usize).min(image.width - 1))
		.collect();
	for y in 0..dh {
		let sy = ((y as f64 / vp.scale) as usize).min(image.height - 1);
		let src = &image.pixels[sy * image.width..][..image.width];
		let dst = &mut out[(y0 + y) * ww + x0..][..dw];
		for (d, &sx) in dst.iter_mut().zip(&xs) {
			*d = src[sx];
		}
	}
	vp
}

/// Alpha-blend `cursor` into a remote-sized 0RGB image with its hotspot at `(x, y)`.
pub fn composite(image: &mut Image, cursor: &CursorShape, x: i32, y: i32) {
	let (left, top) = (x - cursor.hot_x, y - cursor.hot_y);
	for cy in 0..cursor.height {
		let iy = top + cy as i32;
		if iy < 0 || iy >= image.height as i32 {
			continue;
		}
		for cx in 0..cursor.width {
			let ix = left + cx as i32;
			if ix < 0 || ix >= image.width as i32 {
				continue;
			}
			let p = &cursor.rgba[(cy * cursor.width + cx) * 4..][..4];
			let a = p[3] as u32;
			if a == 0 {
				continue;
			}
			let dst = &mut image.pixels[iy as usize * image.width + ix as usize];
			let [_, dr, dg, db] = dst.to_be_bytes();
			let mix = |s: u8, d: u8| ((s as u32 * a + d as u32 * (255 - a)) / 255) as u8;
			*dst = u32::from_be_bytes([0, mix(p[0], dr), mix(p[1], dg), mix(p[2], db)]);
		}
	}
}

pub fn save_png(image: &Image, path: &std::path::Path) -> anyhow::Result<()> {
	let file = std::io::BufWriter::new(std::fs::File::create(path)?);
	let mut encoder = png::Encoder::new(file, image.width as u32, image.height as u32);
	encoder.set_color(png::ColorType::Rgb);
	encoder.set_depth(png::BitDepth::Eight);
	let data: Vec<u8> = image
		.pixels
		.iter()
		.flat_map(|p| p.to_be_bytes()[1..].to_vec())
		.collect();
	encoder.write_header()?.write_image_data(&data)?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn viewport_letterbox() {
		let vp = Viewport::fit((200, 100), (100, 100));
		assert_eq!((vp.scale, vp.offset_x, vp.offset_y), (1.0, 50.0, 0.0));
		assert_eq!(vp.remote_point(50.0, 0.0), (0, 0));
		assert_eq!(vp.remote_point(149.0, 99.0), (99, 99));
		assert_eq!(vp.remote_point(0.0, 500.0), (0, 99));
	}

	#[test]
	fn blit_scales() {
		let image = Image {
			width: 2,
			height: 2,
			pixels: vec![1, 2, 3, 4],
		};
		let mut out = vec![9; 16];
		blit(&image, &mut out, (4, 4));
		assert_eq!(out, vec![1, 1, 2, 2, 1, 1, 2, 2, 3, 3, 4, 4, 3, 3, 4, 4]);
	}

	#[test]
	fn composite_blends_at_hotspot() {
		let mut image = Image {
			width: 3,
			height: 1,
			pixels: vec![0; 3],
		};
		let cursor = CursorShape {
			width: 1,
			height: 1,
			hot_x: 0,
			hot_y: 0,
			rgba: vec![255, 0, 0, 255],
		};
		composite(&mut image, &cursor, 1, 0);
		assert_eq!(image.pixels, vec![0, 0x00ff0000, 0]);
	}
}
