use crate::{audio::Rec, misc::keep_alive, status::MtxStatus, *};
use futures::TryStreamExt;
use matrix_sdk::{
	instant::SystemTime,
	room::Room,
	ruma::events::{
		receipt::{Receipt, ReceiptEventContent},
		room::message::{MessageEventContent, MessageType},
		SyncEphemeralRoomEvent, SyncMessageEvent,
	},
};
use regex::Regex;
use std::{
	io::Cursor,
	path::Path,
	sync::{Arc, Mutex},
};
use tokio::{
	sync::oneshot,
	time::{sleep, sleep_until},
};

static SESSION_PATH: &str = "session.json";

#[tracing::instrument]
fn create_client(hs: &Url) -> Result<Client> {
	let client_config = ClientConfig::default().user_agent(&format!(
		"{}/{}",
		env!("CARGO_CRATE_NAME"),
		env!("CARGO_PKG_VERSION")
	))?;
	let client = Client::new_with_config(hs.clone(), client_config)?;
	//client.store().open_default(CFGDIR.data_dir(), None).context("Open state cache")?;
	Ok(client)
}

#[tracing::instrument]
pub async fn login(args: &Login, config_dir: &Path) -> Result<()> {
	let session_path = config_dir.join(SESSION_PATH);
	anyhow::ensure!(
		args.overwrite || !session_path.exists(),
		"{:?} exists",
		session_path
	);
	let pw;
	let pw = match &args.pw {
		Some(pw) => pw,
		None => {
			let prompt = format!("Login password for {} at {}: ", args.user, args.hs);
			pw = rpassword::read_password_from_tty(Some(&prompt)).context("Read password")?;
			pw.as_str()
		}
	};
	let client = create_client(&args.hs)?;
	let devname = format!(
		"{} on {}",
		env!("CARGO_CRATE_NAME"),
		gethostname().to_string_lossy()
	);
	let login = client
		.login(&args.user, pw, None, Some(&devname))
		.await
		.context("Login")?;
	let session = SessionData {
		homeserver: args.hs.clone(),
		access_token: login.access_token,
		device_id: login.device_id,
		user_id: login.user_id,
	};
	info!(?session, "logged in");
	let mut file = fs::OpenOptions::new();
	file.write(true).truncate(true).create(true);
	#[cfg(unix)]
	{
		use std::os::unix::fs::OpenOptionsExt;
		file.mode(0o600);
	}
	#[cfg(not(unix))]
	warn!(
		session = *session_path,
		"Access token may be world readable"
	);
	let file = file.open(&*session_path).context("Open session file")?;
	serde_json::to_writer_pretty(&file, &session).context("Write session file")?;
	debug!(?file, "success");
	Ok(())
}

#[tracing::instrument]
pub async fn start(config_dir: &Path) -> Result<Client> {
	let session_path = config_dir.join(SESSION_PATH);
	anyhow::ensure!(
		session_path.exists(),
		"Session data does not exist, please login first."
	);
	let sess = File::open(session_path).context("Open session data")?;
	let sess: SessionData = serde_json::from_reader(sess).context("Read session data")?;
	let client = create_client(&sess.homeserver)?;
	client.restore_login(sess.into()).await?;
	debug!(woami=?client.whoami().await, "logged in");
	status::mtx(MtxStatus::Starting);
	let sync = client
		.sync_once(SyncSettings::default())
		.await
		.context("sync")?;
	status::mtx(MtxStatus::Good);
	debug!(?sync, "synced");
	trace!(?sync, "synced");
	Ok(client)
}

#[tracing::instrument(skip(client))]
pub async fn channel(args: &Run, client: &Client) -> Result<JoinedRoom> {
	let chanlist = client.joined_rooms();
	let scl = chanlist
		.iter()
		.map(|c| c.name().unwrap_or_else(|| c.room_id().as_str().to_string()))
		.collect::<Vec<_>>();
	debug!(chanlist=?scl);
	let id = match &args.channel {
		Some(channel) => {
			client
				.join_room_by_id(channel)
				.await
				.context("Join as specified")?;
			if args.leave {
				futures::stream::iter(chanlist.into_iter().filter(|r| r.room_id() != channel))
					.map(|r| async move { r.leave().await })
					.buffer_unordered(5)
					.try_collect::<()>()
					.await
					.context("Leaving superfluous channel")?;
			}
			channel.clone()
		}
		None => match &chanlist[..] {
			[chan] => chan.room_id().clone(),
			[_, ..] => {
				anyhow::bail!(
					"Joined more than one channel: {}. (Specify channel parameter)",
					scl.join(" ")
				);
			}
			[] => match &client.invited_rooms()[..] {
				[] => anyhow::bail!("No channel specified, and joined or invited to any channels."),
				[invitation] => {
					info!(id = ?invitation.room_id(), "Accepting invitation");
					invitation
						.accept_invitation()
						.await
						.context("Accept only invitation")?;
					let timeout = Instant::now() + Duration::from_secs(900);
					client
						.sync_with_callback(SyncSettings::default(), |response| async move {
							let joined = response
								.rooms
								.join
								.iter()
								.any(|(id, _)| id == invitation.room_id());
							trace!(?response, joined);
							match joined {
								true => LoopCtrl::Break,
								false => match Instant::now() > timeout {
									true => {
										error!("Couldn't follow invitation");
										LoopCtrl::Break
									}
									false => LoopCtrl::Continue,
								},
							}
						})
						.await;
					invitation.room_id().clone()
				}
				invs @ [_, ..] => {
					error!(invitations = ?invs
							.iter()
							.map(|c| c.name().unwrap_or_else(|| c.room_id().to_string()))
							.collect::<Vec<_>>(),
						"Invited to more than one channel, refusing all invitations"
					);
					let rej = invs.iter().map(|inv| inv.reject_invitation());
					let rej = futures::stream::iter(rej)
						.buffer_unordered(5)
						.collect::<Vec<_>>()
						.await;
					for rej in rej {
						rej.context("Refuse invitation")?;
					}
					anyhow::bail!("Refused multiple invitations, make sure to invite exactly once");
				}
			},
		},
	};
	let c = client
		.get_joined_room(&id)
		.context("Not joined to a room")?;

	Ok(c)
}

#[tracing::instrument(skip(client, room))]
pub async fn remote_indicator(
	room: JoinedRoom,
	client: Client,
	expect_caught_up_to: Arc<Mutex<Option<SystemTime>>>,
) {
	let here = room.room_id().clone();
	client
		.register_event_handler(
			move |ev: SyncEphemeralRoomEvent<ReceiptEventContent>, room: Room, client: Client| {
				let here = here.clone();
				let ecu = *expect_caught_up_to.lock().unwrap();
				async move {
					let ecu = match ecu {
						Some(ecu) => ecu,
						None => return,
					};
					if room.room_id() != &here {
						return;
					}
					debug!(?ev);
					let u = match room.joined_user_ids().await {
						Ok(u) => u,
						Err(error) => {
							warn!(?error, "Failed to get users for status");
							return;
						}
					};
					let cond = room.topic().and_then(|topic| {
						topic
							.lines()
							.filter_map(|l| l.strip_prefix("gegensprech-markers: "))
							.next()
							.map(Regex::new)
					});
					debug!(?cond, "marker user filtering");
					let cond = cond
						.and_then(|cond| cond.ok())
						.unwrap_or_else(|| Regex::new("").unwrap());
					for u in u {
						if Some(&u) == client.user_id().await.as_ref() {
							continue;
						}
						if !cond.is_match(u.as_str()) {
							continue;
						}
						let rr = room.user_read_receipt(&u).await;
						trace!(?rr, ?u);
						let ts = match rr {
							Ok(Some((_, Receipt { ts: Some(ts), .. }))) => ts,
							Ok(_) => continue,
							Err(err) => {
								warn!(?here, ?u, ?err, "Can't get read receipt");
								continue;
							}
						};
						// This smells like a leap second bug.
						// TODO: Implement proper before or after based on timeline
						if ts
							.to_system_time()
							.expect("Unreasonable UInt of milliseconds")
							< ecu
						{
							debug!(?u, "Not caught up");
							status::caughtup(false);
							return;
						}
					}
					status::caughtup(true);
				}
			},
		)
		.await;
}

#[tracing::instrument(skip(client))]
pub fn oggsender(
	room: JoinedRoom,
	client: Client,
	expect_caught_up_to: Arc<Mutex<Option<SystemTime>>>,
) -> (impl Future<Output = Result<()>>, mpsc::Sender<Rec>) {
	let (tx, mut rx) = mpsc::channel::<Rec>(4);

	let process = async move {
		use matrix_sdk::ruma::events::{
			room::message::{AudioMessageEventContent, MessageType},
			AnyMessageEventContent,
		};

		loop {
			let Rec { data, info } = rx.recv().await.context("recorder sender")?;
			let _sending_status = status::send();
			let data = client
				.upload(
					&info
						.mimetype
						.as_deref()
						.unwrap_or("application/octet-stream")
						.parse()?,
					&mut Cursor::new(data),
				)
				.await?;

			let content = AnyMessageEventContent::RoomMessage(MessageEventContent::new(
				MessageType::Audio(AudioMessageEventContent::plain(
					"Aufnahme".to_owned(),
					data.content_uri,
					Some(info.into()),
				)),
			));

			let txn_id = Uuid::new_v4();
			status::caughtup(false);
			*expect_caught_up_to.lock().unwrap() = Some(SystemTime::now());
			room.send(content, Some(txn_id)).await.unwrap();
		}
	};
	keep_alive(&tx); // Dumb if we exit due to an error elsewhere that'll take us down anyway
	(process, tx)
}

#[tracing::instrument(skip(client))]
pub async fn recv_audio_messages(
	client: &Client,
) -> mpsc::Receiver<(Vec<u8>, Option<String>, oneshot::Sender<()>)> {
	let (tx, rx) = mpsc::channel(4);
	client
		.register_event_handler(
			move |ev: SyncMessageEvent<MessageEventContent>, room: Room, client: Client| {
				let tx = tx.clone();
				async move {
					debug!(?ev, "received");
					if Some(ev.sender) == client.user_id().await {
						return;
					}
					let eid = ev.event_id;
					if let MessageType::Audio(amc) = ev.content.msgtype {
						info!(?amc, "received audio");
						let mtyp = amc
							.info
							.as_ref()
							.and_then(|info| info.mimetype.as_ref())
							.cloned();
						let data = client.get_file(amc, false).await.expect("dl");
						if let Some(data) = data {
							let (play, played) = oneshot::channel();
							tx.send((data, mtyp, play)).await.expect("pipesend");
							tokio::spawn(async move {
								let res = (move || async move {
									let room = match room {
										Room::Joined(j) => Some(j),
										_ => None,
									}
									.context("Room not joined")?;
									let x = played.await;
									x.context("Not played")?;
									room.read_marker(&eid, Some(&eid))
										.await
										.context("Marker request error")?;
									Result::<_, anyhow::Error>::Ok(())
								})()
								.await;
								if let Err(e) = res {
									warn!(?e, "Didn't set read marker")
								}
							});
						} else {
							warn!("audio event, no data file");
						}
					};
				}
			},
		)
		.await;
	rx
}

#[tracing::instrument]
pub async fn sync(client: &Client) {
	let sto = Duration::from_secs(60);
	let ss = SyncSettings::new().timeout(sto);
	let last_sync = Arc::new(Mutex::new(Instant::now()));
	let last_sync_read = last_sync.clone();
	// sync_once doesn't fail because I haven't set a RequestConfig::retry_limit.
	// Setting one has wider implications, so I'll just check the last sync time regularly.

	tokio::spawn(async move {
		let mut doubt = Instant::now();
		loop {
			sleep(Duration::from_secs(10)).await;
			sleep_until(doubt.into()).await;
			doubt = *last_sync_read.lock().unwrap() + sto * 3 / 2;
			match Instant::now() < doubt {
				true => status::mtx(MtxStatus::Good),
				false => status::mtx(MtxStatus::Disconnected),
			}
		}
	});

	client
		.sync_with_callback(ss, move |_| {
			*last_sync.lock().unwrap() = Instant::now();
			async { LoopCtrl::Continue }
		})
		.await;
}
