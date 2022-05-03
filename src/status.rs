use anyhow::{Context, Result};
use apa102_spi::Apa102;
use once_cell::sync::OnceCell;
use rppal::spi;
use smart_leds_trait::{SmartLedsWrite, RGB};
use std::sync::Mutex;
use tracing::error;

use crate::{
	misc::{CallOnDrop, UndoOnDrop},
	Hardware,
};

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

#[derive(Debug)]
struct Status {
	send_status: bool,
	catchup_status: bool,
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
		($name:ident, $r:expr, $g:expr, $b:expr) => {
			pub static $name: RGB8 = RGB8::new($r, $g, $b);
		};
	}
	static H: u8 = 30; // Those LEDs are bit ridiculously bright…
	color!(OFF, 0, 0, 0);
	color!(WEAK_WHITE, 10, 10, 10);
	color!(YELLOW, H, H, 0);
	color!(AMBER, H, H / 2, 0);
	color!(PURPLE, H, 0, H);
	color!(RED, H, 0, 0);
	color!(GREEN, 0, H, 0);
	color!(BLUE, 0, 0, H);
}

struct Seeed(Apa102<spi::Spi>);
impl Seeed {
	fn new() -> Result<Box<Seeed>> {
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
			data[2] = match (status.send_status, status.catchup_status) {
				(true, _) => PURPLE,
				(_, true) => BLUE,
				(_, _) => OFF,
			};
			data[1] = match status.audio_status {
				AudioStatus::Recording => RED,
				AudioStatus::Playing => GREEN,
				AudioStatus::Idle => OFF,
			};
			data[0] = match status.mtx_status {
				MtxStatus::Starting => YELLOW,
				MtxStatus::Good => WEAK_WHITE,
				MtxStatus::Disconnected => AMBER,
			};
		} else {
			data = [OFF; 3];
		}
		self.0.write(data.iter().cloned()).expect("set leds");
	}
}

pub struct StatusIndicators(Mutex<(Box<dyn Render + Send>, Status)>);

static STATUS: OnceCell<StatusIndicators> = OnceCell::new();
static STATUS_INIT: &str = "Status indicator is initialized at start";

impl Status {
	fn initial() -> Status {
		Status {
			send_status: false,
			catchup_status: false,
			mtx_status: MtxStatus::Starting,
			audio_status: AudioStatus::Idle,
			exited: false,
		}
	}
}

// For once I tried designing something that isn't based on a tokio proc and channels
// Oh what have I gotten myself into...

fn status(mut mutate: impl FnMut(&mut Status)) {
	let mut lock = STATUS.get().expect(STATUS_INIT).0.lock().unwrap();
	let (render, status) = &mut *lock;
	mutate(status);
	render.render(status);
}

pub(crate) fn audio(audio: AudioStatus) -> impl UndoOnDrop {
	status(|status| status.audio_status = audio);
	CallOnDrop::call(move || status(|status| status.audio_status = AudioStatus::Idle))
}

pub(crate) fn mtx(mtx: MtxStatus) {
	status(|status| status.mtx_status = mtx);
}

pub(crate) fn send() -> impl UndoOnDrop {
	status(|status| status.send_status = true);
	CallOnDrop::call(move || {
		status(|status| status.send_status = false);
	})
}

#[tracing::instrument]
pub(crate) fn init_from_args(args: &Hardware) -> Result<impl UndoOnDrop> {
	let render: Box<dyn Render + Send> = match args {
		Hardware::Seeed2Mic => Seeed::new()?,
		Hardware::SolderedCustom(_) => Box::new(()),
	};
	if STATUS
		.set(StatusIndicators(Mutex::new((render, Status::initial()))))
		.is_err()
	{
		error!("Can init status LEDs only once");
	}
	status(|_| ());
	Ok(CallOnDrop::call(|| status(|status| status.exited = true)))
}

pub(crate) fn caughtup(caughtup: bool) {
	status(|status| status.catchup_status = !caughtup);
}
