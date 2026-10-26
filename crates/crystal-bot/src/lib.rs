//! A headless zone client: it speaks the zone protocol the way the β client does (session init,
//! Init, FinishLoading, position updates, chat, pings), from the same packet definitions the
//! server uses, and reads back what the server sends. It skips the lobby: the zone takes the actor
//! id game login would have given (`0x1000_0000` + the character's row id).

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, bail};
use bytes::Bytes;
use crystal_net::ipc::{IpcHeader, packet, timestamp};
use crystal_net::{ConnectionType, Frame, FrameCodec, Segment, SegmentType};
use crystal_protocol::zone::{self as ipc, opcode};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio_util::codec::{FramedRead, FramedWrite};

/// What the server sent, decoded.
#[derive(Debug, Clone, PartialEq)]
pub enum Down {
    InitZone {
        territory: u16,
        pos: [f32; 3],
    },
    /// `actor` is the segment's source: the bot's own id for its own spawn.
    Spawn {
        actor: u32,
        spawn: Box<ipc::Spawn>,
    },
    Despawn {
        slot: u8,
        actor: u32,
    },
    Move {
        actor: u32,
        update: ipc::ActorMove,
    },
    Chat(ipc::ChatDown),
    /// The client time the server echoed.
    Pong(u32),
    LogoutReply,
    /// An item of a container batch (NormalItem, GilItem), or a slot update.
    Item(ipc::ItemInfo),
    /// A container batch is complete: sequence, item count, container.
    Container {
        sequence: u32,
        count: u32,
        container: u16,
    },
    Other {
        opcode: u16,
        source: u32,
    },
}

pub struct Bot {
    pub actor: u32,
    reader: FramedRead<OwnedReadHalf, FrameCodec>,
    writer: FramedWrite<OwnedWriteHalf, FrameCodec>,
    /// Decoded packets not yet returned (a frame can hold several).
    pending: std::collections::VecDeque<Down>,
    /// Where the bot stands, after [`Self::enter`] and its own moves.
    pub territory: u16,
    pub pos: [f32; 3],
    /// Every item the server sent, in order, and the containers it closed.
    pub items: Vec<ipc::ItemInfo>,
    pub containers: Vec<(u32, u32, u16)>,
}

impl Bot {
    /// Connects and opens the session (the server's type-2 answer to the session init).
    pub async fn connect(addr: SocketAddr, actor: u32) -> anyhow::Result<Self> {
        let stream = TcpStream::connect(addr).await.with_context(|| format!("connecting to {addr}"))?;
        stream.set_nodelay(true)?;
        let (r, w) = stream.into_split();
        let mut bot = Self {
            actor,
            reader: FramedRead::new(r, FrameCodec),
            writer: FramedWrite::new(w, FrameCodec),
            pending: Default::default(),
            territory: 0,
            pos: [0.0; 3],
            items: Vec::new(),
            containers: Vec::new(),
        };
        let init = Segment::new(SegmentType::SessionInit, 0, 0, ipc::session_init(actor));
        bot.writer.send(Frame::new(ConnectionType::Zone, vec![init])).await?;
        let frame = bot.frame().await?;
        if !frame.segments.iter().any(|s| s.header.segment_type == SegmentType::Unknown(2)) {
            bail!("no answer to the session init");
        }
        Ok(bot)
    }

    /// Init → InitZone → FinishLoading → its own spawn, as the client enters the world.
    pub async fn enter(&mut self) -> anyhow::Result<()> {
        self.send(opcode::INIT, &[0; 0x18]).await?;
        loop {
            if let Down::InitZone { territory, pos } = self.recv().await? {
                (self.territory, self.pos) = (territory, pos);
                break;
            }
        }
        self.loaded().await
    }

    /// FinishLoading, then waits for its own spawn (after an InitZone the bot got otherwise).
    pub async fn loaded(&mut self) -> anyhow::Result<()> {
        self.send(opcode::FINISH_LOADING, &[0; 0x18]).await?;
        loop {
            if let Down::Spawn { actor, spawn } = self.recv().await?
                && actor == self.actor
            {
                self.pos = spawn.pos;
                return Ok(());
            }
        }
    }

    pub async fn move_to(&mut self, pos: [f32; 3], rotation: f32) -> anyhow::Result<()> {
        self.pos = pos;
        self.send(opcode::UPDATE_POSITION, &ipc::PositionUpdate { rotation, pos, ..Default::default() }.to_bytes())
            .await
    }

    pub async fn chat(&mut self, kind: u16, text: &str) -> anyhow::Result<()> {
        let up = ipc::ChatUp { actor: self.actor, pos: self.pos, rotation: 0.0, kind, text: text.into() };
        self.send(opcode::CHAT, &up.to_bytes()).await
    }

    pub async fn ping(&mut self, client_ms: u32) -> anyhow::Result<()> {
        let mut body = [0u8; 0x18];
        body[..4].copy_from_slice(&client_ms.to_le_bytes());
        self.send(opcode::PING, &body).await
    }

    pub async fn logout(&mut self) -> anyhow::Result<()> {
        self.send(opcode::LOGOUT, &[0; 0x18]).await
    }

    pub async fn send(&mut self, op: u16, body: &[u8]) -> anyhow::Result<()> {
        let seg = Segment::new(SegmentType::Ipc, self.actor, self.actor, packet(op, timestamp(), body).freeze());
        self.writer.send(Frame::new(ConnectionType::Zone, vec![seg])).await?;
        Ok(())
    }

    /// The next packet from the server. Cancel-safe.
    pub async fn recv(&mut self) -> anyhow::Result<Down> {
        loop {
            if let Some(d) = self.pending.pop_front() {
                return Ok(d);
            }
            let frame = self.frame().await?;
            for s in &frame.segments {
                if s.header.segment_type != SegmentType::Ipc {
                    continue;
                }
                if let Some(d) = decode(s.header.source, &s.payload) {
                    match &d {
                        Down::Item(i) => self.items.push(*i),
                        Down::Container { sequence, count, container } => {
                            self.containers.push((*sequence, *count, *container))
                        }
                        _ => {}
                    }
                    self.pending.push_back(d);
                }
            }
        }
    }

    /// Waits up to `timeout` for a packet `pick` accepts, skipping the others.
    pub async fn wait_for<T>(
        &mut self,
        timeout: Duration,
        mut pick: impl FnMut(&Down) -> Option<T>,
    ) -> anyhow::Result<T> {
        tokio::time::timeout(timeout, async {
            loop {
                let d = self.recv().await?;
                if let Some(t) = pick(&d) {
                    return Ok(t);
                }
            }
        })
        .await
        .context("timed out")?
    }

    async fn frame(&mut self) -> anyhow::Result<Frame> {
        match self.reader.next().await {
            Some(f) => Ok(f?),
            None => bail!("the server closed the connection"),
        }
    }
}

fn decode(source: u32, payload: &Bytes) -> Option<Down> {
    let header = IpcHeader::parse(payload)?;
    let body = &payload[IpcHeader::SIZE..];
    Some(match header.opcode {
        opcode::INIT_ZONE => {
            let (territory, pos) = ipc::parse_init_zone(body)?;
            Down::InitZone { territory, pos }
        }
        opcode::SPAWN => Down::Spawn { actor: source, spawn: Box::new(ipc::Spawn::parse(body)?) },
        opcode::DESPAWN => {
            let (slot, actor) = ipc::parse_despawn(body)?;
            Down::Despawn { slot, actor }
        }
        opcode::ACTOR_MOVE => Down::Move { actor: source, update: ipc::ActorMove::parse(body)? },
        opcode::CHAT_DOWN => Down::Chat(ipc::ChatDown::parse(body)?),
        opcode::PING => Down::Pong(u32::from_le_bytes(body.get(..4)?.try_into().ok()?)),
        opcode::LOGOUT_REPLY => Down::LogoutReply,
        opcode::NORMAL_ITEM | opcode::GIL_ITEM | opcode::UPDATE_ITEM => Down::Item(ipc::ItemInfo::parse(body)?),
        opcode::ITEM_SIZE => Down::Container {
            sequence: u32::from_le_bytes(body.get(..4)?.try_into().ok()?),
            count: u32::from_le_bytes(body.get(4..8)?.try_into().ok()?),
            container: u16::from_le_bytes(body.get(8..10)?.try_into().ok()?),
        },
        op => Down::Other { opcode: op, source },
    })
}
