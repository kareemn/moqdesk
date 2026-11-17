//! Screen capture of the X11 root window, using MIT-SHM when available.

use anyhow::{Context, Result, bail};
use x11rb::connection::Connection;
use x11rb::protocol::shm::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, Window};
use x11rb::rust_connection::RustConnection;

pub struct Capture {
	conn: RustConnection,
	root: Window,
	width: u16,
	height: u16,
	shm: Option<ShmSegment>,
	fallback: Vec<u8>,
}

struct ShmSegment {
	seg: shm::Seg,
	addr: *mut u8,
	size: usize,
}

// The segment is only touched through &mut Capture.
unsafe impl Send for Capture {}

impl Capture {
	pub fn new(display: Option<&str>) -> Result<Self> {
		let (conn, screen) = x11rb::connect(display).context("failed to connect to the X server")?;
		let setup = conn.setup();
		let screen = &setup.roots[screen];
		let bpp = setup
			.pixmap_formats
			.iter()
			.find(|f| f.depth == screen.root_depth)
			.map(|f| f.bits_per_pixel);
		if screen.root_depth < 24 || bpp != Some(32) {
			bail!(
				"unsupported root visual: depth {} with {:?} bits per pixel",
				screen.root_depth,
				bpp
			);
		}
		let root = screen.root;
		let mut capture = Self {
			conn,
			root,
			width: 0,
			height: 0,
			shm: None,
			fallback: Vec::new(),
		};
		capture.refresh_geometry()?;
		Ok(capture)
	}

	/// Capture size, rounded down to even dimensions for 4:2:0 video.
	pub fn size(&self) -> (u16, u16) {
		(self.width, self.height)
	}

	/// Re-read the root window size (e.g. after a RandR change). Returns true if it changed.
	pub fn refresh_geometry(&mut self) -> Result<bool> {
		let geom = self.conn.get_geometry(self.root)?.reply()?;
		let (width, height) = (geom.width & !1, geom.height & !1);
		if width == 0 || height == 0 {
			bail!("root window is empty");
		}
		if (width, height) == (self.width, self.height) {
			return Ok(false);
		}
		self.width = width;
		self.height = height;
		self.detach();
		match self.attach() {
			Ok(seg) => self.shm = Some(seg),
			Err(err) => tracing::warn!(%err, "MIT-SHM unavailable, falling back to GetImage"),
		}
		Ok(true)
	}

	fn frame_bytes(&self) -> usize {
		self.width as usize * self.height as usize * 4
	}

	fn attach(&self) -> Result<ShmSegment> {
		self.conn.shm_query_version()?.reply().context("no MIT-SHM extension")?;
		let size = self.frame_bytes();
		// SAFETY: plain SysV shm calls; results are checked before use.
		unsafe {
			let id = libc::shmget(libc::IPC_PRIVATE, size, libc::IPC_CREAT | 0o600);
			if id < 0 {
				bail!("shmget failed: {}", std::io::Error::last_os_error());
			}
			let addr = libc::shmat(id, std::ptr::null(), 0);
			if addr as isize == -1 {
				libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut());
				bail!("shmat failed: {}", std::io::Error::last_os_error());
			}
			let seg = self.conn.generate_id()?;
			let attached = self
				.conn
				.shm_attach(seg, id as u32, false)
				.map_err(anyhow::Error::from)
				.and_then(|c| Ok(c.check()?));
			// Mark for removal now; it lives until both we and the X server detach.
			libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut());
			if let Err(err) = attached {
				libc::shmdt(addr);
				return Err(err.context("X server could not attach the segment (remote display?)"));
			}
			Ok(ShmSegment {
				seg,
				addr: addr as *mut u8,
				size,
			})
		}
	}

	fn detach(&mut self) {
		if let Some(seg) = self.shm.take() {
			let _ = self.conn.shm_detach(seg.seg).map(|c| c.check());
			// SAFETY: addr came from shmat and is no longer referenced.
			unsafe { libc::shmdt(seg.addr as *const _) };
		}
	}

	/// Grab the screen as tightly packed BGRX rows, `width * height * 4` bytes.
	pub fn grab(&mut self) -> Result<&[u8]> {
		let (w, h) = (self.width, self.height);
		let format = ImageFormat::Z_PIXMAP.into();
		if let Some(seg) = &self.shm {
			self.conn
				.shm_get_image(self.root, 0, 0, w, h, !0, format, seg.seg, 0)?
				.reply()?;
			// SAFETY: the X server finished writing before replying; size matches the mapping.
			return Ok(unsafe { std::slice::from_raw_parts(seg.addr, seg.size) });
		}
		let reply = self
			.conn
			.get_image(ImageFormat::Z_PIXMAP, self.root, 0, 0, w, h, !0)?
			.reply()?;
		if reply.data.len() < self.frame_bytes() {
			bail!("short GetImage reply");
		}
		self.fallback = reply.data;
		self.fallback.truncate(self.frame_bytes());
		Ok(&self.fallback)
	}
}

impl Drop for Capture {
	fn drop(&mut self) {
		self.detach();
	}
}
