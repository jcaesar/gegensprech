mod audio;
mod button;
mod cmd;
pub mod misc;
mod mtx;
mod status;
use anyhow::{bail, Context, Result};
use directories::ProjectDirs;
use futures::stream::StreamExt;
use gethostname::gethostname;
use matrix_sdk::{
	instant::{Duration, Instant},
	room::Joined as JoinedRoom,
	ruma::{DeviceIdBox, RoomId, UserId},
	uuid::Uuid,
	Client, ClientConfig, LoopCtrl, Session, SyncSettings,
};
use rppal::gpio::Gpio;
use serde::{Deserialize, Serialize};
use std::process::exit;
use std::{
	fs,
	sync::{Arc, Mutex},
};
use std::{fs::File, path::Path};
use std::{future::Future, str::FromStr};
use tokio::{
	signal::unix::{signal, SignalKind},
	sync::mpsc,
};
use tracing::{debug, error, info, trace, warn};
use url::Url;

#[derive(Serialize, Deserialize)]
struct SessionData {
	homeserver: Url,
	access_token: String,
	device_id: DeviceIdBox,
	user_id: UserId,
}

impl std::fmt::Debug for SessionData {
	fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
		fmt.debug_struct("SessionData")
			.field("homeserver", &self.homeserver)
			.field("user_id", &self.user_id)
			.field("device_id", &self.device_id)
			.field(
				"access_token",
				if !self.access_token.is_empty() {
					&"<snip>"
				} else {
					&"<empty>"
				},
			)
			.finish()
	}
}

// https://github.com/tilosp/matrix-send-rs/blob/c3c9edcd61e2730cd0ce0a0258152057a8266d8d/src/matrix.rs <3
impl From<SessionData> for Session {
	fn from(session: SessionData) -> Self {
		Self {
			access_token: session.access_token,
			device_id: session.device_id,
			user_id: session.user_id,
		}
	}
}

structstruck::strike! {
	#[strikethrough[derive(clap::Parser, Debug)]]
	enum Opts {
		/// Generate configuration
		Login(pub struct {
			/// Homeserver URL
			#[clap(short = 's', long)]
			hs: Url,
			/// Login name
			#[clap(short, long)]
			user: String,
			/// Will be read from TTY if possible
			#[clap(short, long)]
			pw: Option<String>,
			/// Do not fail if session file exists
			#[clap(short = 'f', long)]
			overwrite: bool,
		}),
		/// Run normally
		Run(pub struct {
			/// Join channel (wait for invite if not provided)
			#[clap(short, long)]
			channel: Option<RoomId>,
			/// Hardware
			#[clap(subcommand)]
			hardware: enum {
				/// Seeed 2mic HAT
				#[clap(name = "seeed-2mic")]
				Seeed2Mic,
				/// Custom buttons/LEDs
				SolderedCustom(struct {
					/// GPIO button number for control
					#[clap(short, long)]
					button: Option<u8>,
					/// RGB LED at Pins
					///  - Red: Recording
					///  - Green: Playback
					///  - Turquoise: Waiting on Server
					///  - Blue: Waiting for playback by other devices
					///  - Yellow: Connection problem
					///  - White: Idle
					/// If more than three pins are specified, the remaining pins all become ground
					#[clap(short = 'l', long, verbatim_doc_comment)]
					rgb: Option<struct RGBPins {
						r: u8,
						g: u8,
						b: u8,
						ground: Vec<u8>,
					}>,
				}),
			},
		}),
	}
}

impl FromStr for RGBPins {
	type Err = clap::Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let s = s
			.split([',', ':', ' ', '-'])
			.map(str::parse::<u8>)
			.collect::<Result<Vec<_>, _>>()
			.map_err(|e| {
				clap::Error::raw(
					clap::ErrorKind::InvalidValue,
					format!("Couldn't parse as integer: {e:?}"),
				)
			})?;
		if s.len() < 3 {
			return Err(clap::Error::raw(
				clap::ErrorKind::InvalidValue,
				format!("Need at least three pin numbers"),
			));
		}
		let [r, g, b]: [u8; 3] = s[..3].try_into().unwrap();
		let ground = s[3..].iter().cloned().collect();
		Ok(RGBPins { r, g, b, ground })
	}
}

#[tracing::instrument(skip(args))]
async fn run(args: &Run, config_dir: &Path) -> Result<()> {
	let ctrl_c = tokio::signal::ctrl_c();
	let mut term = signal(SignalKind::terminate())?;
	let gpio = Gpio::new().context("Open GPIO for Pins")?;
	let _leds = status::init_from_args(&args.hardware, &gpio).context("Status LED init")?;
	let cmds = cmd::ButtonCommands::load(config_dir)?;
	let client = mtx::start(config_dir).await.context("Matrix startup")?;
	let channel = mtx::channel(args, &client).await.context("Join channel")?;

	let incoming = mtx::recv_audio_messages(&client).await;
	let running_cmd = Arc::new(Mutex::new(None));
	let play = audio::play(incoming, running_cmd.clone());
	let sync = mtx::sync(&client);
	let expect_caught_up_to = Arc::new(Mutex::new(None));
	mtx::remote_indicator(channel.clone(), client.clone(), expect_caught_up_to.clone()).await;
	let (textsender, textchannel) = mtx::oggsender(channel, client.clone(), expect_caught_up_to);
	let button = match args.hardware {
		Hardware::Seeed2Mic => Some(17),
		Hardware::SolderedCustom(SolderedCustom { button, .. }) => button,
	};
	let button = button.map(|button| button::read(button, textchannel, cmds, running_cmd, &gpio));

	tokio::select! {
		() = sync => (),
		e = play => e.context("Audio player")?,
		e = textsender => e.context("Audio sender")?,
		e = button.unwrap(), if button.is_some() => e.context("Button")?,
		_ = ctrl_c => return Ok(()),
		_ = term.recv() => return Ok(()),
	};
	bail!("No task should exit, let alone successfully");
}

#[tokio::main]
async fn main() -> Result<()> {
	tracing_subscriber::fmt::init();
	let config_dir = ProjectDirs::from("de", "liftm", env!("CARGO_CRATE_NAME"))
		.expect("Can't determine settings directory");
	let config_dir = config_dir.config_dir();
	let opts: Opts = clap::Parser::parse();
	debug!("sup");
	debug!(cfg=?config_dir, opts=?opts, "init");
	fs::create_dir_all(config_dir).context("Config dir must exist")?;
	match &opts {
		Opts::Login(args) => mtx::login(args, config_dir).await,
		Opts::Run(args) => run(args, config_dir).await,
	}?;
	exit(0);
}
