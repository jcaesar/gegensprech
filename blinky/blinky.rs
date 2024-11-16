use rppal::spi;
use smart_leds_trait::SmartLedsWrite;
use std::thread::sleep;
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
	let white = smart_leds_trait::RGB8::new(100, 100, 100);
	let off = smart_leds_trait::RGB8::new(100, 100, 100);
	loop {
		for _ in 0..3 {
			for i in 0..3 {
				sleep(Duration::from_millis(50));
				apa102
					.write((0..3).map(|j| match j == i {
						true => white,
						false => off,
					}))
					.expect("Failed to write leds");
			}
		}
		sleep(Duration::from_millis(5000));
	}
}
