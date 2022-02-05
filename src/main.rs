mod button;
mod mtx;
use anyhow::{Context, Result};
use clap::Clap;
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
use serde::{Deserialize, Serialize};
use std::fs;
use std::fs::File;
use std::future::Future;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::{debug, error, info, trace};
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
				if self.access_token.len() > 0 {
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

#[derive(Clap, Debug)]
enum Opts {
	/// Generate configuration
	Login(Login),
	/// Run normally
	Run(Run),
}

#[derive(Clap, Debug)]
pub struct Login {
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
}

#[derive(Clap, Debug)]
pub struct Run {
	/// Join channel (wait for invite if not provided)
	#[clap(short, long)]
	channel: Option<RoomId>,
	/// GPIO button number for control
	#[clap(short, long)]
	button: Option<u8>,
}

lazy_static::lazy_static! {
	static ref CFGDIR: ProjectDirs = ProjectDirs::from("de", "liftm", env!("CARGO_CRATE_NAME"))
		.expect("Can't determine settings directory");
	static ref SESSION: PathBuf = CFGDIR.config_dir().join("session.json");
	static ref OPTS: Opts = Opts::parse();
}

#[tracing::instrument]
async fn run(args: &Run) -> Result<()> {
	let client = mtx::start().await.context("Matrix startup")?;
	let channel = mtx::channel(args, &client).await.context("Join channel")?;

	let syncclient = client.clone(); // internally all Arcs
	let sync = tokio::spawn(async move {
		syncclient
			.sync_with_callback(SyncSettings::default(), |response| async move {
				trace!(?response);
				for (_room_id, room) in response.rooms.join {
					for event in room.timeline.events {
						debug!(?event);
					}
				}
				LoopCtrl::Continue
			})
			.await
	});
	let (textsender, textchannel) = mtx::textsender(channel);
	let morse = args.button.map(|button| button::morse(button, textchannel));

	tokio::select! {
		e = sync => e?,
		e = textsender => e?,
		e = morse.unwrap(), if morse.is_some() => e?,
	};
	unreachable!("No task should exit, let alone successfully");
}

#[tokio::main]
async fn main() -> Result<()> {
	tracing_subscriber::fmt::init();
	debug!("sup");
	debug!(cfg=?*SESSION, opts=?*OPTS, "init");
	fs::create_dir_all((*SESSION).parent().unwrap()).context("Config dir must exist")?;
	match &*OPTS {
		Opts::Login(args) => mtx::login(args).await,
		Opts::Run(args) => run(args).await,
	}
}
