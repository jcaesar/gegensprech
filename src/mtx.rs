use crate::*;

#[tracing::instrument]
fn create_client(hs: &Url) -> Result<Client> {
    let client_config = ClientConfig::default()
        .user_agent(&format!("{}/{}", env!("CARGO_CRATE_NAME"), env!("CARGO_PKG_VERSION")))?;
    let client = Client::new_with_config(hs.clone(), client_config)?;
    //client.store().open_default(CFGDIR.data_dir(), None).context("Open state cache")?;
    Ok(client)
}

#[tracing::instrument]
pub async fn login(args: &Login) -> Result<()> {
    anyhow::ensure!(args.overwrite || !SESSION.exists(), "{:?} exists", *SESSION);
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
    file
        .write(true)
        .truncate(true)
        .create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        file.mode(0o600);
    }
    #[cfg(not(unix))]
    warn!(session = *SESSION, "Access token may be world readable");
    let file = file.open(&*SESSION).context("Open session file")?;
    serde_json::to_writer_pretty(&file, &session).context("Write session file")?;
    debug!(?file, "success");
    Ok(())
}

#[tracing::instrument]
pub async fn start() -> Result<Client> {
    anyhow::ensure!(SESSION.exists(), "Session data does not exist, please login first.");
    let sess = File::open(&*SESSION).context("Open session data")?;
    let sess: SessionData = serde_json::from_reader(sess)
        .context("Read session data")?;
    let client = create_client(&sess.homeserver)?;
    client.restore_login(sess.into()).await?;
    debug!(woami=?client.whoami().await, "logged in");
    let sync = client.sync_once(SyncSettings::default()).await
        .context("sync")?;
    debug!(?sync, "synced");
    trace!(?sync, "synced");
    Ok(client)
}

#[tracing::instrument(skip(client))]
pub async fn channel(args: &Run, client: &Client) -> Result<JoinedRoom> {
    let chanlist = client.joined_rooms();
    let scl = chanlist.iter().map(|c| c.name().unwrap_or(c.room_id().as_str().to_string())).collect::<Vec<_>>();
    debug!(chanlist=?scl);
    let id = match &args.channel {
        Some(channel) => {
            let leave = futures::stream::iter(chanlist.into_iter().filter(|r| r.room_id() != channel)).map(|r| async move { r.leave().await }).buffer_unordered(5).collect::<Vec<_>>().await;
            client.join_room_by_id(channel).await.context("Join as specified")?;
            for leave in leave {
                leave
                    .context("Leaving superfluous channel")?;
            };
            channel.clone()
        },
        None => {
            match &chanlist[..] {
                [chan] => chan.room_id().clone(),
                [_, ..] => {
                    anyhow::bail!("Joined more than one channel: {}. (Specify channel parameter to leave others)", scl.join(" "));
                },
                [] => {
                    match &client.invited_rooms()[..] {
                        [] => anyhow::bail!("No channel specified, and joined or invited to any channels."),
                        [invitation] => {
                            info!(id = ?invitation.room_id(), "Accepting invitation");
                            invitation.accept_invitation().await.context("Accept only invitation")?;
                            let timeout = Instant::now() + Duration::from_secs(900);
                            client.sync_with_callback(SyncSettings::default(), |response| async move {
                                let joined = response.rooms.join.iter().any(|(id, _)| id == invitation.room_id());
                                trace!(?response, joined);
                                match joined {
                                    true => LoopCtrl::Break,
                                    false => match Instant::now() > timeout {
                                        true => {
                                            error!("Coudln't follow invitation");
                                            LoopCtrl::Break
                                        }, false => LoopCtrl::Continue,
                                    },
                                }
                            }).await;
                            invitation.room_id().clone()
                        },
                        invs@[_, ..] => {
                            error!(invitations = ?invs.iter().map(|c| c.name().unwrap_or(c.room_id().to_string())).collect::<Vec<_>>(), "Invited to more than one channel, refusing all invitations");
                            let rej = invs.into_iter().map(|inv| inv.reject_invitation());
                            let rej = futures::stream::iter(rej).buffer_unordered(5).collect::<Vec<_>>().await;
                            for rej in rej {
                                rej.context("Refuse invitation")?;
                            };
                            anyhow::bail!("Refused multiple invitations, make sure to invite exactly once");
                        }
                    }
                },
            }
        },
    };
    Ok(client.get_joined_room(&id).context("Not joined to a room")?)
}

pub fn textsender(room: JoinedRoom) -> (impl Future<Output = Result<()>>, mpsc::Sender<String>) {
    let (tx, mut rx) = mpsc::channel(128);

    let process = async move {
        use matrix_sdk::ruma::events::{
            AnyMessageEventContent,
            room::message::{MessageEventContent},
        };

        loop {
            let content = AnyMessageEventContent::RoomMessage(
                MessageEventContent::text_plain(rx.recv().await.context("channel")?)
            );
    
            let txn_id = Uuid::new_v4();
            room.send(content, Some(txn_id)).await.unwrap();
        }
    };
   (process, tx) 
}
