//! MoQ session: subscribes to the host's video and cursor tracks and publishes input.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use moqdesk_proto::{CURSOR_TRACK, CursorUpdate, INPUT_TRACK, InputEvent, VIDEO_TRACK, VIEWER_PREFIX};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::decode::Packet;

pub struct Target {
	pub url: url::Url,
	pub name: String,
	pub fingerprint: Option<String>,
	pub insecure: bool,
}

pub enum Update {
	Cursor(CursorUpdate),
	Status(String),
}

pub async fn run(
	target: Target,
	input: UnboundedReceiver<InputEvent>,
	packets: mpsc::Sender<Packet>,
	updates: impl Fn(Update) + Send + Sync + 'static,
) -> Result<()> {
	let local = moq_tokio::origin::spawn();
	let path = format!("{VIEWER_PREFIX}/{}", random_id());
	let broadcast = local
		.create_broadcast(&path)
		.context("failed to create viewer broadcast")?;
	broadcast.announce(Default::default())?;
	let info = moq_net::track::Info::default()
		.with_timescale(moq_net::Timescale::MICRO)
		.with_priority(u8::MAX)
		.with_max_age(Duration::from_secs(2));
	let input_track = broadcast.create_track(INPUT_TRACK, info)?;
	tokio::spawn(publish_input(input_track, input));

	let remote = moq_tokio::origin::spawn();
	let mut config = moq_tokio::connect::Config::default();
	if let Some(fp) = target.fingerprint {
		config.tls.fingerprint = vec![fp];
	}
	if target.insecure {
		config.tls.insecure = Some(true);
	}
	let client = config
		.init(Default::default())?
		.with_publisher(&local)
		.with_subscriber(remote.clone());
	updates(Update::Status(format!(
		"connecting to {}",
		target.url.host_str().unwrap_or_default()
	)));
	let connection = client.connect(target.url);

	let consumer = remote.consume();
	let watch = async {
		let mut announced = consumer.announced();
		loop {
			let update = announced.next().await.context("origin closed")?;
			if update.prefix.to_string() != target.name || !update.kind.is_active() {
				continue;
			}
			updates(Update::Status("connected".into()));
			let desk = consumer.request_broadcast(target.name.as_str()).await?;
			let res = tokio::select! {
				res = video(&desk, &packets) => res,
				res = cursor(&desk, &updates) => res,
			};
			tracing::info!(?res, "desktop went away");
			updates(Update::Status("waiting for host".into()));
		}
	};
	tokio::select! {
		res = connection.closed() => res.context("connection closed"),
		res = watch => res,
	}
}

fn random_id() -> String {
	let mut bytes = [0u8; 8];
	getrandom::fill(&mut bytes).expect("no OS randomness");
	bytes.iter().map(|b| format!("{b:02x}")).collect()
}

async fn publish_input(track: moq_net::track::Producer, mut rx: UnboundedReceiver<InputEvent>) -> Result<()> {
	let epoch = Instant::now();
	let mut group: Option<(moq_net::group::Producer, Instant)> = None;
	while let Some(event) = rx.recv().await {
		// Rotate groups so the host's bounded subscription never has to buffer a long history.
		if group
			.as_ref()
			.is_none_or(|(_, started)| started.elapsed() > Duration::from_secs(1))
		{
			if let Some((old, _)) = group.take() {
				let _ = old.finish();
			}
			group = Some((track.append_group()?, Instant::now()));
		}
		let (g, _) = group.as_mut().unwrap();
		let ts = moq_net::Timestamp::new(epoch.elapsed().as_micros() as u64, moq_net::Timescale::MICRO)?;
		g.write_frame(ts, event.encode())?;
	}
	Ok(())
}

/// Follow the live edge: whenever a newer group (GOP) arrives, abandon the current one.
async fn video(desk: &moq_net::broadcast::Consumer, packets: &mpsc::Sender<Packet>) -> Result<()> {
	let mut sub = desk.track(VIDEO_TRACK)?.subscribe(None).await?;
	let mut current: Option<(u64, moq_net::group::Consumer, bool)> = None;
	loop {
		tokio::select! {
			biased;
			group = sub.recv_group() => {
				let Some(group) = group? else { return Ok(()) };
				let seq = group.sequence;
				if current.as_ref().is_none_or(|(cur, _, _)| seq > *cur) {
					current = Some((seq, group, true));
				}
			}
			frame = async { current.as_mut().unwrap().1.read_frame().await }, if current.is_some() => {
				let Some(frame) = frame? else {
					current = None;
					continue;
				};
				let (seq, _, first) = current.as_mut().unwrap();
				let frame = hang::container::Frame::decode(frame.payload)?;
				let packet = Packet { group: *seq, keyframe: std::mem::take(first), data: frame.payload };
				if packets.send(packet).is_err() {
					return Ok(());
				}
			}
		}
	}
}

async fn cursor(desk: &moq_net::broadcast::Consumer, updates: &impl Fn(Update)) -> Result<()> {
	let mut sub = desk.track(CURSOR_TRACK)?.subscribe(None).await?;
	let mut current: Option<(u64, moq_net::group::Consumer)> = None;
	loop {
		tokio::select! {
			biased;
			group = sub.recv_group() => {
				let Some(group) = group? else { return Ok(()) };
				let seq = group.sequence;
				if current.as_ref().is_none_or(|(cur, _)| seq > *cur) {
					current = Some((seq, group));
				}
			}
			frame = async { current.as_mut().unwrap().1.read_frame().await }, if current.is_some() => {
				let Some(frame) = frame? else {
					current = None;
					continue;
				};
				match CursorUpdate::decode(&frame.payload) {
					Ok(update) => {
						if let CursorUpdate::Shape { width, height, hot_x, hot_y, .. } = &update {
							tracing::debug!(width, height, hot_x, hot_y, "cursor shape");
						}
						updates(Update::Cursor(update))
					}
					Err(err) => tracing::warn!(%err, "bad cursor message"),
				}
			}
		}
	}
}
