//! The Noise IK handshake and the transport session it yields (R1-01).
//!
//! ```text
//!  client (device)                                   host (daemon)
//!  initiator(device_static, host_pub, carrier, id)   responder(host_static, carrier, id)
//!     │ msg1: e, es, s, ss   (payload empty)  ───────▶ read_message: prologue, pinned
//!     │                                                key and device key all checked
//!     │ ◀─────── msg2: e, ee, se   (payload empty)     write_message
//!     ▼                                                ▼
//!  into_session() ──▶ Session  ◀══ frames ══▶  Session ◀── into_session()
//! ```
//!
//! Both sides mix the [`crate::prologue`] into the handshake hash. A client
//! that dialed the wrong carrier or the wrong host id, or pinned a key the
//! host does not hold, therefore fails at message 1 with
//! [`NoiseError::Decrypt`]: the host reads no frame from it. Handshake
//! payloads are empty in version 1; a message that carries one is refused.
//!
//! One Noise transport message decrypts to exactly one [`Frame`]. A
//! [`Session`] can be [`split`](Session::split) into a [`Sealer`] and an
//! [`Opener`] so a reader task and a writer task each own their half with no
//! lock: the nonce of each direction is counted by the half that uses it.

use std::sync::Arc;

use ainb_hangar_proto::hosts::{CarrierKind, HostId};
use snow::params::NoiseParams;
use snow::resolvers::{CryptoResolver, DefaultResolver};
use snow::{Builder, HandshakeState, StatelessTransportState};

use crate::frame::HeaderError;
use crate::prologue::{PrologueError, prologue};
use crate::reassembly::{Frame, TAG_LEN};
use crate::{MAX_NOISE_MESSAGE, NOISE_PATTERN};

/// An X25519 key length.
pub const KEY_LEN: usize = 32;

/// Why a handshake or a transport message failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoiseError {
    /// The prologue could not be built (a `local` host id, an unknown
    /// carrier).
    Prologue(PrologueError),
    /// A message did not decrypt or authenticate: a wrong prologue, a key the
    /// other side does not hold, a tampered, replayed or reordered message.
    /// Fatal: the host closes 4401.
    Decrypt,
    /// A handshake message carried a payload; version 1 sends none. Fatal.
    HandshakePayload(usize),
    /// A call out of turn: writing when it is the other side's turn, reading
    /// after the handshake finished, or a session from an unfinished
    /// handshake.
    State(&'static str),
    /// A message or frame larger than one Noise message.
    TooLarge(usize),
    /// The direction's nonce is used up. Fatal.
    Exhausted,
    /// The plaintext is not a frame. [`HeaderError::UnknownOpcode`] is NOT
    /// fatal: the receiver drops the frame and counts it. Any other header
    /// error is.
    Frame(HeaderError),
    /// The crypto library refused for another reason.
    Crypto(String),
}

impl NoiseError {
    /// Whether the session must close. Only a frame with an unknown opcode
    /// leaves it usable.
    #[must_use]
    pub const fn is_fatal(&self) -> bool {
        !matches!(self, Self::Frame(HeaderError::UnknownOpcode(_)))
    }
}

impl std::fmt::Display for NoiseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Prologue(e) => write!(f, "noise prologue: {e}"),
            Self::Decrypt => f.write_str("noise message did not decrypt"),
            Self::HandshakePayload(n) => write!(f, "handshake message carried a {n}-byte payload"),
            Self::State(what) => write!(f, "noise state: {what}"),
            Self::TooLarge(n) => write!(f, "{n} bytes do not fit one noise message"),
            Self::Exhausted => f.write_str("noise nonce exhausted"),
            Self::Frame(e) => write!(f, "noise frame: {e}"),
            Self::Crypto(e) => write!(f, "noise: {e}"),
        }
    }
}

impl std::error::Error for NoiseError {}

impl From<snow::Error> for NoiseError {
    fn from(e: snow::Error) -> Self {
        match e {
            snow::Error::Decrypt => Self::Decrypt,
            other => Self::Crypto(other.to_string()),
        }
    }
}

fn params() -> NoiseParams {
    // The pattern is a constant this crate owns; a parse failure is a build
    // defect, caught by every test in this file.
    NOISE_PATTERN
        .parse()
        .unwrap_or_else(|e| unreachable!("{NOISE_PATTERN} does not parse: {e}"))
}

/// An X25519 static key pair.
#[derive(Clone, PartialEq, Eq)]
pub struct Keypair {
    /// The private key. Never logged.
    pub private: [u8; KEY_LEN],
    /// The public key.
    pub public: [u8; KEY_LEN],
}

impl std::fmt::Debug for Keypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keypair")
            .field("private", &"<redacted>")
            .field("public", &self.public)
            .finish()
    }
}

fn key(bytes: &[u8]) -> Result<[u8; KEY_LEN], NoiseError> {
    bytes
        .try_into()
        .map_err(|_| NoiseError::Crypto(format!("key is {} bytes, not {KEY_LEN}", bytes.len())))
}

/// A fresh static key pair from the operating system's random source.
pub fn generate_keypair() -> Result<Keypair, NoiseError> {
    let pair = Builder::new(params()).generate_keypair()?;
    Ok(Keypair {
        private: key(&pair.private)?,
        public: key(&pair.public)?,
    })
}

/// The public key of an X25519 private key, for a key read back from
/// custody.
pub fn public_key(private: &[u8; KEY_LEN]) -> Result<[u8; KEY_LEN], NoiseError> {
    let mut dh = DefaultResolver
        .resolve_dh(&params().dh)
        .ok_or(NoiseError::State("no X25519 implementation"))?;
    dh.set(private);
    key(dh.pubkey())
}

/// A handshake in progress.
pub struct Handshake {
    state: HandshakeState,
}

impl std::fmt::Debug for Handshake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Handshake")
            .field("initiator", &self.state.is_initiator())
            .field("finished", &self.state.is_handshake_finished())
            .finish_non_exhaustive()
    }
}

/// The client side: this device's static key, the host key it pinned from
/// the pairing offer, and the carrier and host id it dialed.
pub fn initiator(
    device_static: &[u8; KEY_LEN],
    host_static_pub: &[u8; KEY_LEN],
    carrier: CarrierKind,
    host_id: &HostId,
) -> Result<Handshake, NoiseError> {
    let prologue = prologue(carrier, host_id).map_err(NoiseError::Prologue)?;
    let state = Builder::new(params())
        .local_private_key(device_static)
        .remote_public_key(host_static_pub)
        .prologue(&prologue)
        .build_initiator()?;
    Ok(Handshake { state })
}

/// The host side: the host's static key, and the carrier and host id of the
/// listener the socket arrived on.
pub fn responder(
    host_static: &[u8; KEY_LEN],
    carrier: CarrierKind,
    host_id: &HostId,
) -> Result<Handshake, NoiseError> {
    let prologue = prologue(carrier, host_id).map_err(NoiseError::Prologue)?;
    let state = Builder::new(params())
        .local_private_key(host_static)
        .prologue(&prologue)
        .build_responder()?;
    Ok(Handshake { state })
}

impl Handshake {
    /// Write this side's next handshake message (empty payload).
    pub fn write_message(&mut self) -> Result<Vec<u8>, NoiseError> {
        if self.state.is_handshake_finished() || !self.state.is_my_turn() {
            return Err(NoiseError::State("not this side's turn to write"));
        }
        let mut out = vec![0u8; MAX_NOISE_MESSAGE];
        let len = self.state.write_message(&[], &mut out)?;
        out.truncate(len);
        Ok(out)
    }

    /// Read the other side's next handshake message. A payload is refused.
    pub fn read_message(&mut self, message: &[u8]) -> Result<(), NoiseError> {
        if self.state.is_handshake_finished() || self.state.is_my_turn() {
            return Err(NoiseError::State("not this side's turn to read"));
        }
        if message.len() > MAX_NOISE_MESSAGE {
            return Err(NoiseError::TooLarge(message.len()));
        }
        let mut payload = vec![0u8; MAX_NOISE_MESSAGE];
        let len = self.state.read_message(message, &mut payload)?;
        if len != 0 {
            return Err(NoiseError::HandshakePayload(len));
        }
        Ok(())
    }

    /// Whether both messages have passed.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.state.is_handshake_finished()
    }

    /// The other side's static key: on the host, the device key, known once
    /// message 1 is read (the token check binds to it); on the client, the
    /// pinned host key.
    #[must_use]
    pub fn remote_static(&self) -> Option<[u8; KEY_LEN]> {
        self.state.get_remote_static().and_then(|k| k.try_into().ok())
    }

    /// The transport session. The handshake must be finished.
    pub fn into_session(self) -> Result<Session, NoiseError> {
        if !self.state.is_handshake_finished() {
            return Err(NoiseError::State("handshake is not finished"));
        }
        let remote_static =
            self.remote_static().ok_or(NoiseError::State("no remote static key"))?;
        let handshake_hash = self.state.get_handshake_hash().to_vec();
        let transport = Arc::new(self.state.into_stateless_transport_mode()?);
        Ok(Session {
            sealer: Sealer {
                transport: Arc::clone(&transport),
                nonce: 0,
            },
            opener: Opener {
                transport,
                nonce: 0,
            },
            remote_static,
            handshake_hash,
        })
    }
}

/// A finished handshake: frames in both directions.
#[derive(Debug)]
pub struct Session {
    sealer: Sealer,
    opener: Opener,
    remote_static: [u8; KEY_LEN],
    handshake_hash: Vec<u8>,
}

impl Session {
    /// Encrypt one frame into one Noise message.
    pub fn encrypt_frame(&mut self, frame: &Frame) -> Result<Vec<u8>, NoiseError> {
        self.sealer.seal(frame)
    }

    /// Decrypt one Noise message into one frame.
    pub fn decrypt_frame(&mut self, message: &[u8]) -> Result<Frame, NoiseError> {
        self.opener.open(message)
    }

    /// The other side's static key.
    #[must_use]
    pub const fn remote_static(&self) -> [u8; KEY_LEN] {
        self.remote_static
    }

    /// The Noise handshake hash, the same on both sides: a channel binding
    /// for anything that must prove it rode this session.
    #[must_use]
    pub fn handshake_hash(&self) -> &[u8] {
        &self.handshake_hash
    }

    /// The two directions, for a reader task and a writer task.
    #[must_use]
    pub fn split(self) -> (Sealer, Opener) {
        (self.sealer, self.opener)
    }
}

/// The sending half of a session.
pub struct Sealer {
    transport: Arc<StatelessTransportState>,
    nonce: u64,
}

impl std::fmt::Debug for Sealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sealer").field("nonce", &self.nonce).finish_non_exhaustive()
    }
}

impl Sealer {
    /// Encrypt one frame into one Noise message.
    pub fn seal(&mut self, frame: &Frame) -> Result<Vec<u8>, NoiseError> {
        self.seal_plaintext(&frame.encode())
    }

    fn seal_plaintext(&mut self, plain: &[u8]) -> Result<Vec<u8>, NoiseError> {
        if plain.len() + TAG_LEN > MAX_NOISE_MESSAGE {
            return Err(NoiseError::TooLarge(plain.len() + TAG_LEN));
        }
        // Noise reserves the last nonce value.
        if self.nonce == u64::MAX {
            return Err(NoiseError::Exhausted);
        }
        let mut out = vec![0u8; plain.len() + TAG_LEN];
        let len = self.transport.write_message(self.nonce, plain, &mut out)?;
        out.truncate(len);
        self.nonce += 1;
        Ok(out)
    }
}

/// The receiving half of a session.
pub struct Opener {
    transport: Arc<StatelessTransportState>,
    nonce: u64,
}

impl std::fmt::Debug for Opener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Opener").field("nonce", &self.nonce).finish_non_exhaustive()
    }
}

impl Opener {
    /// Decrypt one Noise message into one frame.
    ///
    /// A message that does not decrypt leaves the nonce where it was, so the
    /// session is dead ([`NoiseError::Decrypt`] is fatal). A message that
    /// decrypts but holds an unknown opcode has used its nonce; the session
    /// goes on.
    pub fn open(&mut self, message: &[u8]) -> Result<Frame, NoiseError> {
        if message.len() > MAX_NOISE_MESSAGE {
            return Err(NoiseError::TooLarge(message.len()));
        }
        if message.len() < TAG_LEN {
            return Err(NoiseError::Decrypt);
        }
        if self.nonce == u64::MAX {
            return Err(NoiseError::Exhausted);
        }
        let mut plain = vec![0u8; message.len()];
        let len = self.transport.read_message(self.nonce, message, &mut plain)?;
        self.nonce += 1;
        Frame::decode(&plain[..len]).map_err(NoiseError::Frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opcode::Opcode;

    const HOST: &str = "01K5A0000000000000000ABCDE";

    fn pair() -> (Session, Session) {
        let host = generate_keypair().unwrap();
        let device = generate_keypair().unwrap();
        let id = HostId::parse(HOST).unwrap();
        let mut i = initiator(&device.private, &host.public, CarrierKind::Lan, &id).unwrap();
        let mut r = responder(&host.private, CarrierKind::Lan, &id).unwrap();
        r.read_message(&i.write_message().unwrap()).unwrap();
        i.read_message(&r.write_message().unwrap()).unwrap();
        (i.into_session().unwrap(), r.into_session().unwrap())
    }

    /// A handshake message with a payload: built with snow directly, since
    /// this crate's own writer never sends one.
    #[test]
    fn a_handshake_payload_is_refused() {
        let host = generate_keypair().unwrap();
        let device = generate_keypair().unwrap();
        let id = HostId::parse(HOST).unwrap();
        let prologue = prologue(CarrierKind::Lan, &id).unwrap();
        let mut raw = Builder::new(params())
            .local_private_key(&device.private)
            .remote_public_key(&host.public)
            .prologue(&prologue)
            .build_initiator()
            .unwrap();
        let mut msg = vec![0u8; 1024];
        let len = raw.write_message(b"hello", &mut msg).unwrap();
        let mut r = responder(&host.private, CarrierKind::Lan, &id).unwrap();
        assert_eq!(
            r.read_message(&msg[..len]),
            Err(NoiseError::HandshakePayload(5))
        );
    }

    #[test]
    fn an_unknown_opcode_is_dropped_and_the_session_goes_on() {
        let (mut client, mut host) = pair();
        let mut bad = Frame::control(Opcode::Rpc, 0, Vec::new()).encode();
        bad[2] = 12;
        let sealed = client.sealer.seal_plaintext(&bad).unwrap();
        let err = host.decrypt_frame(&sealed).unwrap_err();
        assert_eq!(err, NoiseError::Frame(HeaderError::UnknownOpcode(12)));
        assert!(!err.is_fatal());
        let ping = Frame::control(Opcode::Ping, 1, Vec::new());
        let sealed = client.encrypt_frame(&ping).unwrap();
        assert_eq!(host.decrypt_frame(&sealed), Ok(ping));
    }

    #[test]
    fn an_exhausted_nonce_is_refused_both_ways() {
        let (mut client, mut host) = pair();
        client.sealer.nonce = u64::MAX;
        let ping = Frame::control(Opcode::Ping, 1, Vec::new());
        assert_eq!(client.encrypt_frame(&ping), Err(NoiseError::Exhausted));
        host.opener.nonce = u64::MAX;
        assert_eq!(host.decrypt_frame(&[0u8; 32]), Err(NoiseError::Exhausted));
    }
}
