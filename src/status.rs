use anyhow::{Context, Result};
use apa102_spi::Apa102;
use once_cell::sync::OnceCell;
use rppal::spi;
use smart_leds_trait::{SmartLedsWrite, RGB};
use std::sync::Mutex;
use tracing::error;

use crate::Hardware;

#[derive(Debug, Clone, Copy)]
pub enum AudioStatus {
	Recording,
	Playing,
	Idle,
}

#[derive(Debug, Clone, Copy)]
pub enum MtxStatus {
	Starting,
	Good,
	Disconnected,
}

#[derive(Debug, Clone, Copy)]
pub enum SendStatus {
	Uploading,
	WaitingForReceipt,
	Normal,
}

#[derive(Debug)]
struct Status {
	send_status: SendStatus,
	mtx_status: MtxStatus,
	audio_status: AudioStatus,
	exited: bool,
}

trait Render {
	fn render(&mut self, status: &Status);
}

impl Render for () {
	fn render(&mut self, _status: &Status) {}
}

mod led_color {
	use smart_leds_trait::RGB8;

	macro_rules! color {
		($name:ident, $r:tt, $g:tt, $b:tt) => {
			pub static $name: RGB8 = RGB8::new($r, $g, $b);
		};
	}
	static H: u8 = 30; // Those LEDs are bit ridiculously bright…
	color!(OFF, 0, 0, 0);
	color!(WEAK_WHITE, 10, 10, 10);
	color!(YELLOW, H, H, 0);
	color!(PURPLE, H, 0, H);
	color!(RED, H, 0, 0);
	color!(GREEN, 0, H, 0);
	color!(BLUE, 0, 0, H);
}

struct Seeed(Apa102<spi::Spi>);
impl Seeed {
	fn new() -> Result<Box<dyn Render + Send>> {
		let spi = spi::Spi::new(
			spi::Bus::Spi0,
			spi::SlaveSelect::Ss1,
			8000000,
			spi::Mode::Mode0,
		)
		.context("Open Spi0 Ss1")?;
		let apa102 = apa102_spi::Apa102::new(spi);
		Ok(Box::new(Seeed(apa102)))
	}
}

impl Render for Seeed {
	#[tracing::instrument(skip(self))]
	fn render(&mut self, status: &Status) {
		use led_color::*;
		let mut data = [RGB::<u8>::default(); 3];
		if !status.exited {
			data[2] = match status.send_status {
				SendStatus::Uploading => PURPLE,
				SendStatus::WaitingForReceipt => BLUE,
				SendStatus::Normal => OFF,
			};
			data[1] = match status.audio_status {
				AudioStatus::Recording => RED,
				AudioStatus::Playing => GREEN,
				AudioStatus::Idle => OFF,
			};
			data[0] = match status.mtx_status {
				MtxStatus::Starting => YELLOW,
				MtxStatus::Good => WEAK_WHITE,
				MtxStatus::Disconnected => YELLOW,
			};
		} else {
			data = [OFF; 3];
		}
		self.0.write(data.iter().cloned()).expect("set leds");
	}
}

pub struct StatusIndicators(Mutex<(Box<dyn Render + Send>, Status)>);

static STATUS: OnceCell<StatusIndicators> = OnceCell::new();
static STATUS_INIT: &'static str = "Status indicator is initialized at start";

impl Status {
	fn initial() -> Status {
		Status {
			send_status: SendStatus::Normal,
			mtx_status: MtxStatus::Starting,
			audio_status: AudioStatus::Idle,
			exited: false,
		}
	}
}

// For once I tried designing something that isn't based on a tokio proc and channels
// Oh what have I gotten myself into...
pub trait UndoOnDrop {}
struct CallOnDrop<T: FnOnce()>(Option<T>);
impl<T: FnOnce()> UndoOnDrop for CallOnDrop<T> {}

impl<T: FnOnce()> Drop for CallOnDrop<T> {
	fn drop(&mut self) {
		self.0.take().unwrap()();
	}
}

fn status(mut mutate: impl FnMut(&mut Status)) {
	let mut lock = STATUS.get().expect(STATUS_INIT).0.lock().unwrap();
	let (render, status) = &mut *lock;
	mutate(status);
	render.render(status);
}

pub(crate) fn audio(audio: AudioStatus) -> impl UndoOnDrop {
	status(|status| status.audio_status = audio);
	CallOnDrop(Some(move || {
		status(|status| status.audio_status = AudioStatus::Idle)
	}))
}

pub(crate) fn mtx(mtx: MtxStatus) {
	status(|status| status.mtx_status = mtx);
}

pub(crate) fn send(send: SendStatus) {
	status(|status| status.send_status = send);
}

#[tracing::instrument]
pub(crate) fn init_from_args(args: &Hardware) -> Result<impl UndoOnDrop> {
	let render = match args {
		Hardware::Seeed2Mic => Seeed::new()?,
		Hardware::SolderedCustom(_) => Box::new(()),
	};
	if let Err(_) = STATUS.set(StatusIndicators(Mutex::new((render, Status::initial())))) {
		error!("Can init status LEDs only once");
	}
	status(|_| ());
	Ok(CallOnDrop(Some(|| status(|status| status.exited = true))))
}
