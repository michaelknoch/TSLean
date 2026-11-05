use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use futures::prelude::*;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tsclientlib::data::{Channel, Client, Connection as Book};
use tsclientlib::events::{Event, PropertyId, PropertyValue};
use tsclientlib::prelude::*;
use tsclientlib::{
    AudioEvent, ChannelId, ChannelPermissionHint, ClientId, ClientPermissionHint, ClientType, Connection,
    DisconnectOptions, Identity, MessageTarget, Reason, StreamItem, UidBuf,
};
use tsproto_packets::packets::{AudioData, OutPacket};

use crate::audio::{Controls, Playback, Sound};

const CHAT_HISTORY: usize = 500;
const TALK_TIMEOUT: Duration = Duration::from_millis(300);
const ICON_MAX_BYTES: u64 = 256 * 1024;
/// Ids below this are icons bundled with the official client, not stored on the server.
pub const BUILTIN_ICON_MAX: u32 = 1000;

pub struct Login {
    pub address: String,
    pub nickname: String,
    pub password: String,
    pub identity: Identity,
    pub input_muted: bool,
    pub output_muted: bool,
    pub volumes: HashMap<String, f32>,
}

pub enum Cmd {
    Connect(Login),
    Disconnect,
    Quit,
    /// `None` moves the own client.
    Move(Option<ClientId>, ChannelId, Option<String>),
    MoveChannel {
        id: ChannelId,
        target: ChannelId,
        after: bool,
    },
    EditChannel {
        id: ChannelId,
        name: Option<String>,
        topic: Option<String>,
        password: Option<String>,
    },
    Kick(ClientId, Reason),
    Chat(ChatTab, String),
    SetMuted {
        input: bool,
        output: bool,
    },
    OpenDm(UidBuf, String),
    CloseDm(UidBuf),
    Playback(rtrb::Producer<Playback>),
    Volume(UidBuf, f32),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ChatTab {
    Channel,
    Server,
    Private(UidBuf),
}

#[derive(Clone)]
pub struct Line {
    pub time: String,
    pub from: Option<String>,
    pub text: String,
}

#[derive(Clone)]
pub struct Chat {
    pub tab: ChatTab,
    pub title: String,
    pub lines: Arc<VecDeque<Line>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SpacerAlign {
    Left,
    Center,
    Right,
    Rule,
}

pub struct Badge {
    pub icon: u32,
    pub name: String,
}

pub enum Row {
    Channel {
        id: ChannelId,
        title: String,
        name: String,
        depth: usize,
        spacer: Option<SpacerAlign>,
        locked: bool,
        topic: String,
        perms: ChannelPermissionHint,
    },
    Client {
        id: ClientId,
        channel: ChannelId,
        uid: Option<UidBuf>,
        label: String,
        depth: usize,
        muted: bool,
        away: bool,
        badges: Vec<Badge>,
        perms: ClientPermissionHint,
    },
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Disconnected,
    Connecting,
    Connected,
}

/// Immutable snapshot published to the UI; every heavy field is behind an `Arc`.
#[derive(Clone, Default)]
pub struct State {
    pub status: Status,
    pub error: Option<String>,
    pub server_name: String,
    pub own_id: Option<ClientId>,
    pub own_channel: Option<ChannelId>,
    pub input_muted: bool,
    pub output_muted: bool,
    pub tree: Arc<Vec<Row>>,
    pub chats: Arc<Vec<Chat>>,
    pub own_channel_name: String,
    pub talking: Arc<Vec<ClientId>>,
}

pub type Mic = (OutPacket, bool);

#[derive(Default)]
pub struct Shared {
    pub state: Mutex<Arc<State>>,
    /// Taken by the UI and persisted; set whenever the identity's security level grows.
    pub identity: Mutex<Option<Identity>>,
    /// Downloaded group icon files by icon id, drained by the UI.
    pub icons: Mutex<Vec<(u32, eframe::egui::ColorImage)>>,
}

impl State {
    fn chat_mut(&mut self, tab: &ChatTab, title: impl FnOnce() -> String) -> &mut Chat {
        let chats = Arc::make_mut(&mut self.chats);
        let i = match chats.iter().position(|c| &c.tab == tab) {
            Some(i) => i,
            None => {
                chats.push(Chat { tab: tab.clone(), title: title(), lines: Default::default() });
                chats.len() - 1
            }
        };
        &mut chats[i]
    }

    fn log(&mut self, tab: ChatTab, title: &str, from: Option<&str>, text: impl Into<String>) {
        let chat = self.chat_mut(&tab, || title.to_string());
        let lines = Arc::make_mut(&mut chat.lines);
        if lines.len() == CHAT_HISTORY {
            lines.pop_front();
        }
        lines.push_back(Line { time: clock(), from: from.map(str::to_string), text: text.into() });
    }

    fn system(&mut self, text: impl Into<String>) {
        self.log(ChatTab::Server, "Server", None, text);
    }
}

fn clock() -> String {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    unsafe { libc::localtime_r(&now, &mut tm) };
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

pub struct Net {
    shared: Arc<Shared>,
    state: State,
    ctx: eframe::egui::Context,
    controls: Arc<Controls>,
    playback: Option<rtrb::Producer<Playback>>,
    talk_seen: HashMap<ClientId, Instant>,
    volumes: HashMap<String, f32>,
    icons_requested: HashSet<u32>,
    icon_downloads: HashMap<u16, u32>,
}

pub fn spawn(
    shared: Arc<Shared>,
    ctx: eframe::egui::Context,
    controls: Arc<Controls>,
    mic: mpsc::Receiver<Mic>,
) -> (mpsc::UnboundedSender<Cmd>, std::thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let net = Net {
        shared,
        state: State::default(),
        ctx,
        controls,
        playback: None,
        talk_seen: HashMap::new(),
        volumes: HashMap::new(),
        icons_requested: HashSet::new(),
        icon_downloads: HashMap::new(),
    };
    let handle = std::thread::Builder::new()
        .name("net".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(net.run(rx, mic));
        })
        .unwrap();
    (tx, handle)
}

enum Step {
    Cmd(Option<Cmd>),
    Mic(Mic),
    Event(Option<Result<StreamItem, tsclientlib::Error>>),
    Tick,
}

impl Net {
    fn publish(&mut self) {
        *self.shared.state.lock().unwrap() = Arc::new(self.state.clone());
        self.ctx.request_repaint();
    }

    async fn run(mut self, mut cmds: mpsc::UnboundedReceiver<Cmd>, mut mic: mpsc::Receiver<Mic>) {
        let mut next = None;
        loop {
            let login = match next.take() {
                Some(l) => l,
                None => tokio::select! {
                    c = cmds.recv() => match c {
                        Some(Cmd::Connect(l)) => l,
                        Some(Cmd::Playback(p)) => { self.playback = Some(p); continue }
                        Some(Cmd::Quit) | None => return,
                        Some(_) => continue,
                    },
                    Some(_) = mic.recv() => continue,
                },
            };
            self.state.status = Status::Connecting;
            self.state.error = None;
            self.publish();

            let res = self.session(login, &mut cmds, &mut mic, &mut next).await;
            let quit = matches!(res, Ok(true));
            self.controls.can_send.store(false, Relaxed);
            self.reset_playback();
            let s = &mut self.state;
            s.status = Status::Disconnected;
            s.tree = Default::default();
            s.talking = Default::default();
            s.own_id = None;
            s.own_channel = None;
            match res {
                Ok(_) => s.system("Disconnected"),
                Err(e) => {
                    s.system(format!("Disconnected: {e}"));
                    s.error = Some(e.to_string());
                }
            }
            self.publish();
            if quit {
                return;
            }
        }
    }

    /// Returns `Ok(true)` when the app is quitting.
    async fn session(
        &mut self,
        login: Login,
        cmds: &mut mpsc::UnboundedReceiver<Cmd>,
        mic: &mut mpsc::Receiver<Mic>,
        next: &mut Option<Login>,
    ) -> Result<bool> {
        let mut opts = Connection::build(login.address)
            .logger(slog::Logger::root(slog::Discard, slog::o!()))
            .name(login.nickname)
            .identity(login.identity)
            .input_muted(login.input_muted)
            .output_muted(login.output_muted);
        if !login.password.is_empty() {
            opts = opts.password(login.password);
        }
        self.volumes = login.volumes;
        self.icons_requested.clear();
        self.icon_downloads.clear();
        let mut con = opts.connect()?;
        let mut tick = tokio::time::interval(TALK_TIMEOUT);

        let quit = loop {
            let step = {
                let mut events = con.events();
                tokio::select! {
                    c = cmds.recv() => Step::Cmd(c),
                    Some(p) = mic.recv() => Step::Mic(p),
                    e = events.next() => Step::Event(e),
                    _ = tick.tick(), if !self.talk_seen.is_empty() => Step::Tick,
                }
            };
            match step {
                Step::Cmd(None | Some(Cmd::Quit)) => break true,
                Step::Cmd(Some(Cmd::Disconnect)) => break false,
                Step::Cmd(Some(Cmd::Connect(l))) => {
                    *next = Some(l);
                    break false;
                }
                Step::Cmd(Some(c)) => self.command(&mut con, c)?,
                Step::Mic((p, end)) => {
                    if con.can_send_audio() {
                        let _ = con.send_audio(p);
                    }
                    if let Ok(own) = con.get_state().map(|b| b.own_client) {
                        self.talker(own, !end);
                    }
                }
                Step::Event(None) => return Err(anyhow!("connection closed")),
                Step::Event(Some(Err(e))) => return Err(e.into()),
                Step::Event(Some(Ok(item))) => self.event(&mut con, item)?,
                Step::Tick => self.expire_talkers(),
            }
        };

        con.disconnect(DisconnectOptions::new())?;
        let _ = tokio::time::timeout(Duration::from_secs(2), con.events().for_each(|_| future::ready(()))).await;
        Ok(quit)
    }

    fn command(&mut self, con: &mut Connection, cmd: Cmd) -> Result<()> {
        if let Cmd::Playback(p) = cmd {
            self.playback = Some(p);
            return Ok(());
        }
        let Ok(book) = con.get_state() else { return Ok(()) };
        match cmd {
            Cmd::Move(client, channel, password) => {
                let Some(c) = book.clients.get(&client.unwrap_or(book.own_client)) else { return Ok(()) };
                let mut part = c.client_move(channel);
                if let Some(pw) = &password {
                    part = part.set_password(pw);
                }
                part.send_with_result(con)?;
            }
            Cmd::MoveChannel { id, target, after } => {
                let (Some(ch), Some(t)) = (book.channels.get(&id), book.channels.get(&target)) else { return Ok(()) };
                ch.channel_move(t.parent, if after { target } else { t.order }).send_with_result(con)?;
            }
            Cmd::EditChannel { id, name, topic, password } => {
                let Some(ch) = book.channels.get(&id) else { return Ok(()) };
                let mut part = ch.edit();
                if let Some(name) = &name {
                    part = part.set_name(name);
                }
                if let Some(topic) = &topic {
                    part = part.set_topic(topic);
                }
                if let Some(pw) = &password {
                    part = part.set_password(Some(pw));
                }
                part.send_with_result(con)?;
            }
            Cmd::Kick(client, reason) => {
                let Some(c) = book.clients.get(&client) else { return Ok(()) };
                c.kick(reason).send_with_result(con)?;
            }
            Cmd::Chat(tab, text) => {
                let target = match &tab {
                    ChatTab::Server => MessageTarget::Server,
                    ChatTab::Channel => MessageTarget::Channel,
                    ChatTab::Private(uid) => match book.clients.values().find(|c| c.uid.as_ref() == Some(uid)) {
                        Some(c) => MessageTarget::Client(c.id),
                        None => {
                            self.state.log(tab, "", None, "This user is offline");
                            self.publish();
                            return Ok(());
                        }
                    },
                };
                book.send_message(target, &text).send(con)?;
            }
            Cmd::SetMuted { input, output } => {
                book.client_update().set_input_muted(input).set_output_muted(output).send(con)?;
            }
            Cmd::OpenDm(uid, title) => {
                self.state.chat_mut(&ChatTab::Private(uid), || title);
                self.publish();
            }
            Cmd::CloseDm(uid) => {
                Arc::make_mut(&mut self.state.chats).retain(|c| c.tab != ChatTab::Private(uid.clone()));
                self.publish();
            }
            Cmd::Volume(uid, v) => {
                let id = book.clients.values().find(|c| c.uid.as_ref() == Some(&uid)).map(|c| c.id);
                self.volumes.insert(uid.to_string(), v);
                if let (Some(id), Some(p)) = (id, &mut self.playback) {
                    let _ = p.push(Playback::Volume(id, v));
                }
            }
            Cmd::Connect(_) | Cmd::Disconnect | Cmd::Quit | Cmd::Playback(_) => {}
        }
        Ok(())
    }

    fn event(&mut self, con: &mut Connection, item: StreamItem) -> Result<()> {
        match item {
            StreamItem::BookEvents(events) => {
                let reconnected =
                    events.iter().any(|e| matches!(e, Event::PropertyAdded { id: PropertyId::Server, .. }));
                if reconnected {
                    con.get_state()?.server.set_subscribed(true).send(con)?;
                    self.state.status = Status::Connected;
                    self.state.error = None;
                }
                let book = con.get_state()?;
                let joined = events.iter().any(|e| matches!(e, Event::PropertyAdded { id: PropertyId::Client(_), .. }));
                if joined && !self.volumes.is_empty() {
                    for c in book.clients.values() {
                        let v = c.uid.as_ref().and_then(|u| self.volumes.get(&u.to_string()));
                        if let (Some(v), Some(p)) = (v, &mut self.playback) {
                            let _ = p.push(Playback::Volume(c.id, *v));
                        }
                    }
                }
                let mut sound = reconnected.then_some(Sound::SelfJoin);
                for e in &events {
                    sound = log_event(&mut self.state, book, e, reconnected).or(sound);
                }
                if let (Some(sound), Some(p)) = (sound, &mut self.playback) {
                    if !book.clients.get(&book.own_client).is_some_and(|c| c.output_muted) {
                        let _ = p.push(Playback::Sound(sound));
                    }
                }
                if !events.iter().all(|e| matches!(e, Event::Message { .. })) {
                    let own = book.clients.get(&book.own_client);
                    self.state.server_name = book.server.name.clone();
                    self.state.own_id = Some(book.own_client);
                    self.state.own_channel = own.map(|c| c.channel);
                    self.state.own_channel_name =
                        own.and_then(|c| book.channels.get(&c.channel)).map(|c| c.name.clone()).unwrap_or_default();
                    self.state.input_muted = own.is_some_and(|c| c.input_muted);
                    self.state.output_muted = own.is_some_and(|c| c.output_muted);
                    self.state.tree = Arc::new(build_tree(book));
                }
                let groups_added = events.iter().any(|e| {
                    matches!(
                        e,
                        Event::PropertyAdded { id: PropertyId::ServerGroup(_) | PropertyId::ChannelGroup(_), .. }
                    )
                });
                if groups_added {
                    let icons: Vec<u32> = (book.server_groups.values().map(|g| g.icon))
                        .chain(book.channel_groups.values().map(|g| g.icon))
                        .map(|i| i.0)
                        .filter(|&i| i >= BUILTIN_ICON_MAX && self.icons_requested.insert(i))
                        .collect();
                    for icon in icons {
                        if let Ok(h) = con.download_file(ChannelId(0), &format!("/icon_{icon}"), None, None) {
                            self.icon_downloads.insert(h.0, icon);
                        }
                    }
                }
                self.publish();
            }
            StreamItem::Audio(packet) => {
                let (from, end) = match packet.data().data() {
                    AudioData::S2C { from, data, .. } | AudioData::S2CWhisper { from, data, .. } => {
                        (ClientId(*from), data.len() <= 1)
                    }
                    _ => return Ok(()),
                };
                if let Some(p) = &mut self.playback {
                    let _ = p.push(Playback::Packet(from, packet));
                }
                self.talker(from, !end);
            }
            StreamItem::AudioChange(AudioEvent::CanSendAudio(b)) => self.controls.can_send.store(b, Relaxed),
            StreamItem::AudioChange(AudioEvent::CanReceiveAudio(false)) => {
                self.reset_playback();
                self.talk_seen.clear();
                self.publish_talkers();
            }
            StreamItem::DisconnectedTemporarily(reason) => {
                self.state.status = Status::Connecting;
                self.state.system(format!("Connection lost ({reason:?}), reconnecting…"));
                self.publish();
            }
            StreamItem::MessageResult(_, Err(e)) => {
                self.state.system(e.to_string());
                self.publish();
            }
            StreamItem::FileDownload(h, mut dl) => {
                let Some(icon) = self.icon_downloads.remove(&h.0).filter(|_| dl.size <= ICON_MAX_BYTES) else {
                    return Ok(());
                };
                let (shared, ctx) = (self.shared.clone(), self.ctx.clone());
                tokio::spawn(async move {
                    let mut bytes = Vec::with_capacity(dl.size as usize);
                    if dl.stream.read_to_end(&mut bytes).await.is_err() {
                        return;
                    }
                    if let Ok(img) = eframe::icon_data::from_png_bytes(&bytes) {
                        let size = [img.width as usize, img.height as usize];
                        let image = eframe::egui::ColorImage::from_rgba_unmultiplied(size, &img.rgba);
                        shared.icons.lock().unwrap().push((icon, image));
                        ctx.request_repaint();
                    }
                });
            }
            StreamItem::FiletransferFailed(h, _) => {
                self.icon_downloads.remove(&h.0);
            }
            StreamItem::IdentityLevelIncreased => {
                *self.shared.identity.lock().unwrap() = con.get_options().get_identity().cloned();
                self.ctx.request_repaint();
            }
            _ => {}
        }
        Ok(())
    }

    fn reset_playback(&mut self) {
        if let Some(p) = &mut self.playback {
            let _ = p.push(Playback::Reset);
        }
    }

    fn talker(&mut self, id: ClientId, talking: bool) {
        let changed = if talking {
            self.talk_seen.insert(id, Instant::now()).is_none()
        } else {
            self.talk_seen.remove(&id).is_some()
        };
        if changed {
            self.publish_talkers();
        }
    }

    fn expire_talkers(&mut self) {
        let before = self.talk_seen.len();
        self.talk_seen.retain(|_, t| t.elapsed() < TALK_TIMEOUT);
        if self.talk_seen.len() != before {
            self.publish_talkers();
        }
    }

    fn publish_talkers(&mut self) {
        self.state.talking = Arc::new(self.talk_seen.keys().copied().collect());
        self.publish();
    }
}

/// `initial` marks the batch that fills a fresh book, whose additions are not real joins.
fn log_event(s: &mut State, book: &Book, e: &Event, initial: bool) -> Option<Sound> {
    let own_ch = book.clients.get(&book.own_client).map(|c| c.channel);
    let name_of = |ch: &ChannelId| book.channels.get(ch).map(|c| c.name.as_str()).unwrap_or("?");
    match e {
        Event::Message { target, invoker, message } => {
            let text = strip_bbcode(message);
            match target {
                MessageTarget::Server => s.log(ChatTab::Server, "Server", Some(&invoker.name), text),
                MessageTarget::Channel => s.log(ChatTab::Channel, "Channel", Some(&invoker.name), text),
                MessageTarget::Poke(_) => s.system(format!("{} poked you: {text}", invoker.name)),
                MessageTarget::Client(to) => {
                    let other = if invoker.id == book.own_client {
                        book.clients.get(to)
                    } else {
                        book.clients.get(&invoker.id)
                    };
                    if let Some(c) = other {
                        if let Some(uid) = &c.uid {
                            s.log(ChatTab::Private(uid.clone()), &c.name, Some(&invoker.name), text);
                        }
                    }
                }
            }
        }
        Event::PropertyAdded { id: PropertyId::Client(id), .. } if !initial => {
            let c = book.clients.get(id).filter(|c| c.client_type == ClientType::Normal && *id != book.own_client)?;
            s.system(format!("{} connected", c.name));
            return (own_ch == Some(c.channel)).then_some(Sound::Join);
        }
        Event::PropertyRemoved { old: PropertyValue::Client(c), .. } if c.client_type == ClientType::Normal => {
            s.system(format!("{} disconnected", c.name));
            return (own_ch == Some(c.channel)).then_some(Sound::Leave);
        }
        Event::PropertyChanged { id: PropertyId::ClientChannel(id), old: PropertyValue::ChannelId(from), .. } => {
            let c = book.clients.get(id)?;
            if *id == book.own_client {
                s.system(format!("You joined {}", name_of(&c.channel)));
                return Some(Sound::SelfJoin);
            } else if own_ch == Some(c.channel) {
                s.log(ChatTab::Channel, "Channel", None, format!("{} joined", c.name));
                return Some(Sound::Join);
            } else if own_ch == Some(*from) {
                s.log(ChatTab::Channel, "Channel", None, format!("{} left for {}", c.name, name_of(&c.channel)));
                return Some(Sound::Leave);
            }
        }
        _ => {}
    }
    None
}

fn build_tree(book: &Book) -> Vec<Row> {
    let mut children: HashMap<ChannelId, Vec<&Channel>> = HashMap::new();
    for ch in book.channels.values() {
        children.entry(ch.parent).or_default().push(ch);
    }
    for list in children.values_mut() {
        order_siblings(list);
    }
    let mut clients: HashMap<ChannelId, Vec<&Client>> = HashMap::new();
    for c in book.clients.values().filter(|c| c.client_type == ClientType::Normal) {
        clients.entry(c.channel).or_default().push(c);
    }
    for list in clients.values_mut() {
        list.sort_by_cached_key(|c| (std::cmp::Reverse(c.talk_power), c.name.to_lowercase()));
    }
    let mut rows = Vec::with_capacity(book.channels.len() + book.clients.len());
    push_channels(&mut rows, book, &children, &clients, ChannelId(0), 0);
    rows
}

fn push_channels(
    rows: &mut Vec<Row>,
    book: &Book,
    children: &HashMap<ChannelId, Vec<&Channel>>,
    clients: &HashMap<ChannelId, Vec<&Client>>,
    parent: ChannelId,
    depth: usize,
) {
    for ch in children.get(&parent).into_iter().flatten() {
        let (spacer, name) = match parse_spacer(&ch.name).filter(|_| parent == ChannelId(0)) {
            Some((align, text)) => (Some(align), text.to_uppercase()),
            None => (None, ch.name.to_uppercase()),
        };
        rows.push(Row::Channel {
            id: ch.id,
            title: ch.name.clone(),
            name,
            depth,
            spacer,
            locked: ch.has_password.unwrap_or(false),
            topic: ch.topic.clone().unwrap_or_default(),
            perms: ch.permission_hints.unwrap_or(ChannelPermissionHint::all()),
        });
        for c in clients.get(&ch.id).into_iter().flatten() {
            let label = match c.away_message.as_deref() {
                Some("") => format!("{}  · away", c.name),
                Some(msg) => format!("{}  · {msg}", c.name),
                None => c.name.clone(),
            };
            rows.push(Row::Client {
                id: c.id,
                channel: c.channel,
                uid: c.uid.clone(),
                label,
                depth: depth + 1,
                muted: c.input_muted || !c.input_hardware_enabled || c.output_muted || !c.output_hardware_enabled,
                away: c.away_message.is_some(),
                badges: badges(book, c),
                perms: c.permission_hints.unwrap_or(ClientPermissionHint::all()),
            });
        }
        push_channels(rows, book, children, clients, ch.id, depth + 1);
    }
}

fn badges(book: &Book, c: &Client) -> Vec<Badge> {
    let mut groups: Vec<_> = c.server_groups.iter().filter_map(|g| book.server_groups.get(g)).collect();
    groups.sort_by_key(|g| g.sort_id);
    let channel_group = book.channel_groups.get(&c.channel_group).map(|g| (g.icon, &g.name));
    (channel_group.into_iter())
        .chain(groups.into_iter().map(|g| (g.icon, &g.name)))
        .filter(|(icon, _)| icon.0 != 0)
        .map(|(icon, name)| Badge { icon: icon.0, name: name.clone() })
        .collect()
}

fn order_siblings(list: &mut Vec<&Channel>) {
    let mut ordered = Vec::with_capacity(list.len());
    let mut prev = ChannelId(0);
    while let Some(i) = list.iter().position(|c| c.order == prev) {
        let c = list.swap_remove(i);
        prev = c.id;
        ordered.push(c);
    }
    list.sort_by_key(|c| c.id.0);
    ordered.append(list);
    *list = ordered;
}

fn parse_spacer(name: &str) -> Option<(SpacerAlign, &str)> {
    let rest = name.strip_prefix('[')?;
    let close = rest.find(']')?;
    let (kind, _) = rest[..close].split_once("spacer")?;
    let align = match kind {
        "" | "l" => SpacerAlign::Left,
        "c" => SpacerAlign::Center,
        "r" => SpacerAlign::Right,
        "*" => SpacerAlign::Rule,
        _ => return None,
    };
    Some((align, rest[close + 1..].trim()))
}

fn strip_bbcode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        match rest[i..].find(']') {
            Some(j) if is_bbcode_tag(&rest[i + 1..i + j]) => rest = &rest[i + j + 1..],
            _ => {
                out.push('[');
                rest = &rest[i + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn is_bbcode_tag(tag: &str) -> bool {
    let tag = tag.trim_start_matches('/').to_ascii_lowercase();
    let name = tag.split('=').next().unwrap_or("");
    matches!(name, "b" | "i" | "u" | "s" | "url" | "color" | "size" | "img" | "center" | "left" | "right")
}
