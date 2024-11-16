use rppal::spi;
use signal_hook::{consts::SIGHUP, consts::SIGINT, consts::SIGTERM, iterator::Signals};
use smart_leds_trait::SmartLedsWrite;
use std::convert::Infallible;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn main() {
	let spi = spi::Spi::new(
		spi::Bus::Spi0,
		spi::SlaveSelect::Ss1,
		8000000,
		spi::Mode::Mode0,
	)
	.expect("Failed to open Spi0 Ss1");
	let mut apa102 = apa102_spi::Apa102::new(spi);
	let (send, recv) = mpsc::channel::<Infallible>();
	let mut signals =
		Signals::new([SIGINT, SIGTERM, SIGHUP]).expect("Failed to set up signal handler");
	thread::spawn(move || {
		signals.forever().next();
		drop(send);
	});
	let white = smart_leds_trait::RGB8::new(100, 100, 100);
	let off = smart_leds_trait::RGB8::new(0, 0, 0);
	loop {
		for _ in 0..3 {
			for i in 0..3 {
				apa102
					.write((0..3).map(|j| match j == i {
						true => white,
						false => off,
					}))
					.expect("Failed to write leds");
				thread::sleep(Duration::from_millis(50));
			}
			apa102
				.write((0..3).map(|_| off))
				.expect("Failed to write leds");
		}
		if matches!(
			recv.recv_timeout(Duration::from_millis(5000)),
			Err(mpsc::RecvTimeoutError::Disconnected)
		) {
			break;
		};
	}
}
