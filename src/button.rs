use rppal::gpio::Trigger;
use rppal::gpio::Gpio;
use rppal::gpio::Level;
use rppal::gpio::Pin;
use rppal::gpio::InputPin;
use rppal::system::DeviceInfo;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::mpsc::Sender;
use anyhow::Result;

#[derive(Debug, PartialEq, Eq)]
enum PinPoll { Edge, Timeout }
#[derive(Debug, PartialEq, Eq)]
enum Press {
    Short(Instant),
    LongStart(Instant),
    LongEnd(Instant, Instant),
}

struct EdgeRaw {
    pin: InputPin,
}
struct EdgeDeb {
    raw: EdgeRaw,
    lrs: Level,
    deb: Duration,
}
struct Button {
    edge: EdgeDeb,
    lpd: Duration,
    longdown: Option<(bool, Instant)>,
}

impl EdgeRaw {
    fn new(pin: Pin) -> Result<Self> {
        let mut pin = pin.into_input_pullup();
        pin.set_interrupt(Trigger::Both)?;
        Ok(Self { pin })
    }
    fn current(&self) -> Level {
        self.pin.read()
    }
    #[tracing::instrument(skip(self))]
    fn next(&mut self, timeout: Option<Instant>) -> Result<bool> {
        let ret = match timeout {
            Some(timeout) => match timeout.checked_duration_since(Instant::now()) {
                sleep@Some(_) => self.pin.poll_interrupt(false, sleep)?,
                None => None,
            }, None => self.pin.poll_interrupt(false, None)?
        }.is_some();
        tracing::trace!(button_edge = ?ret);
        Ok(ret)
    }
}
impl EdgeDeb {
    fn new(pin: Pin) -> Result<Self> {
        let raw = EdgeRaw::new(pin)?;
        let lrs = Level::High;
        Ok(Self { raw, lrs, deb: Duration::from_millis(10) })
    }
    #[tracing::instrument(skip(self))]
    fn next(&mut self, timeout: Option<Instant>) -> Result<(PinPoll, Level, Instant)> {
        let mut pf = Instant::now();
        loop {
            let level = self.raw.current();
            tracing::trace!(?level, ?self.lrs);
            if level != self.lrs {
                self.lrs = level;
                tracing::debug!(?level, ?pf, "edge");
                return Ok((PinPoll::Edge, level, pf));
            };
            let edge = self.raw.next(timeout)?;
            pf = Instant::now();
            if !edge {
                tracing::debug!(?level, ?pf, "timeout");
                return Ok((PinPoll::Timeout, level, pf));
            };
            loop {
                let timeout = Instant::now() + self.deb;
                let edge = self.raw.next(Some(timeout))?;
                tracing::trace!(?edge, ?timeout);
                match edge {
                    true => continue,
                    false => break,
                }   
            }
        }
    }
}
impl Button {
    fn new(edge: EdgeDeb) -> Self {
        Self { edge, lpd: Duration::from_millis(250), longdown: None }
    }
    #[tracing::instrument(skip(self))]
    fn next(&mut self, timeout: Option<Instant>) -> Result<Option<Press>> {
        loop {
            match self.longdown {
                Some((false, down)) if down + self.lpd < Instant::now() => {
                    tracing::trace!("got long");
                    self.longdown = Some((true, down));
                    return Ok(Some(Press::LongStart(down)));
                },
                Some((false, down)) => {
                    let (edge, level, _time) = self.edge.next(Some(down + self.lpd))?;
                    tracing::trace!(?edge, ?level, "still short");
                    match edge {
                        PinPoll::Timeout => {
                            self.longdown = Some((true, down));
                            return Ok(Some(Press::LongStart(down)));
                        }, PinPoll::Edge => {
                            assert!(level == Level::High);
                            self.longdown = None;
                            return Ok(Some(Press::Short(down)));
                        },
                    }
                },
                Some(_) | None => {
                    let (edge, level, time) = self.edge.next(timeout)?;
                    tracing::trace!(?edge, ?level, ?self.longdown, "new edge");
                    match edge {
                        PinPoll::Timeout => return Ok(None),
                        PinPoll::Edge => match self.longdown {
                            None => {
                                assert!(level == Level::Low);
                                self.longdown = Some((false, time));
                            },
                            Some((true, start)) => {
                                assert!(level == Level::High);
                                self.longdown = None;
                                return Ok(Some(Press::LongEnd(start, time)));
                            },
                            _ => unreachable!()
                        }
                    }
                },
            }
        }
    }
}

#[tracing::instrument(skip(messages))]
pub async fn morse(button: u8, messages: Sender<String>) -> Result<()> {
    tracing::info!(raspi=?DeviceInfo::new());
    let mut button = Button::new(EdgeDeb::new(Gpio::new()?.get(button)?)?);
    tokio::task::spawn_blocking(move || -> Result<()> {
        loop {
            let mut message = "Morsies: ".to_string();
            let mut timeout = false;
            loop {
                let et = button.next(match timeout {
                    true => Some(Instant::now() + Duration::from_secs(2)),
                    false => None,
                })?;
                tracing::trace!(?message, ?et);
                message += match et {
                    Some(Press::Short(_)) => { timeout = true; "・" }, 
                    Some(Press::LongStart(_)) => { timeout = false; "﹘" },
                    Some(Press::LongEnd(_, _)) => { timeout = true; "" },
                    None => break,
                };
            }
            tracing::debug!(?message, "send");
            messages.blocking_send(message)?;
        }
    }).await??;
    Ok(())
}
