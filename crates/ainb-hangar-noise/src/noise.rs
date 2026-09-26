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
//!
//! IK message 1 can be REPLAYED: anyone who saw it can send it again, and the
//! host will read it and answer. So the host learns nothing it may act on
//! from message 1. The device key is handed out only by
//! [`Opener::remote_static`] once a transport frame from the device has
//! opened, which a replayer cannot produce.

use std::sync::Arc;

use ainb_hangar_proto::hosts::{CarrierKind, HostId};
use snow::params::NoiseParams;
use snow::resolvers::{CryptoResolver, DefaultResolver};
use snow::{Builder, HandshakeState, StatelessTransportState};
use zeroize::{Zeroize, ZeroizeOnDrop};

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
    /// Message 1 named a low-order device static key ([`is_low_order`]): a
    /// session keyed on it is not secret. Fatal: the host closes 4401.
    LowOrderKey,
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
            Self::LowOrderKey => f.write_str("handshake named a low-order static key"),
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

/// An X25519 static key pair. Zeroed when dropped.
///
/// No `PartialEq`: comparing private keys with `==` is not constant time, and
/// nothing needs it. Compare `public` where an identity check is wanted.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
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
    let mut pair = Builder::new(params()).generate_keypair()?;
    let keys = key(&pair.private).and_then(|private| {
        Ok(Keypair {
            private,
            public: key(&pair.public)?,
        })
    });
    pair.private.zeroize();
    keys
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

/// Whether `public` is a low-order X25519 point.
///
/// Such a point's Diffie-Hellman output is all zero whatever the other side's
/// key, so a session keyed on it is not secret. The host refuses one at
/// message 1 and at redeem (R1-07).
///
/// The check multiplies `public` by a clamped scalar, which is a multiple of
/// the cofactor 8, so the product is zero exactly when `public` lies in the
/// small subgroup (non-canonical encodings of those points included).
#[must_use]
pub fn is_low_order(public: &[u8; KEY_LEN]) -> bool {
    let Some(mut dh) = DefaultResolver.resolve_dh(&params().dh) else {
        return true;
    };
    dh.set(&[0x5a; KEY_LEN]);
    let mut out = [0u8; KEY_LEN];
    if dh.dh(public, &mut out).is_err() {
        return true;
    }
    out.iter().all(|b| *b == 0)
}

/// F2: the host's check on the device static key message 1 named.
fn refuse_low_order(remote: Option<[u8; KEY_LEN]>) -> Result<(), NoiseError> {
    match remote {
        Some(key) if is_low_order(&key) => Err(NoiseError::LowOrderKey),
        _ => Ok(()),
    }
}

/// A handshake in progress.
pub struct Handshake {
    state: HandshakeState,
    /// Set once a message was refused; every later call fails.
    refused: bool,
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
    Ok(Handshake {
        state,
        refused: false,
    })
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
    Ok(Handshake {
        state,
        refused: false,
    })
}

impl Handshake {
    /// Write this side's next handshake message (empty payload).
    pub fn write_message(&mut self) -> Result<Vec<u8>, NoiseError> {
        if self.refused {
            return Err(NoiseError::State("handshake was refused"));
        }
        if self.state.is_handshake_finished() || !self.state.is_my_turn() {
            return Err(NoiseError::State("not this side's turn to write"));
        }
        let mut out = vec![0u8; MAX_NOISE_MESSAGE];
        let len = self.state.write_message(&[], &mut out)?;
        out.truncate(len);
        Ok(out)
    }

    /// Read the other side's next handshake message. A payload is refused,
    /// and on the host so is a low-order device static key in message 1.
    pub fn read_message(&mut self, message: &[u8]) -> Result<(), NoiseError> {
        if self.refused {
            return Err(NoiseError::State("handshake was refused"));
        }
        if self.state.is_handshake_finished() || self.state.is_my_turn() {
            return Err(NoiseError::State("not this side's turn to read"));
        }
        if message.len() > MAX_NOISE_MESSAGE {
            return Err(NoiseError::TooLarge(message.len()));
        }
        let mut payload = vec![0u8; MAX_NOISE_MESSAGE];
        let len = self.state.read_message(message, &mut payload)?;
        if len != 0 {
            self.refused = true;
            return Err(NoiseError::HandshakePayload(len));
        }
        if !self.state.is_initiator() {
            refuse_low_order(self.raw_remote_static()).inspect_err(|_| self.refused = true)?;
        }
        Ok(())
    }

    /// Whether both messages have passed.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.state.is_handshake_finished()
    }

    fn raw_remote_static(&self) -> Option<[u8; KEY_LEN]> {
        self.state.get_remote_static().and_then(|k| k.try_into().ok())
    }

    /// The transport session. The handshake must be finished.
    pub fn into_session(self) -> Result<Session, NoiseError> {
        if self.refused {
            return Err(NoiseError::State("handshake was refused"));
        }
        if !self.state.is_handshake_finished() {
            return Err(NoiseError::State("handshake is not finished"));
        }
        let remote_static =
            self.raw_remote_static().ok_or(NoiseError::State("no remote static key"))?;
        // The client pinned the host key before it dialed, so that key is
        // known. The host's view of the device key is proven only by the
        // first frame that opens.
        let proven = self.state.is_initiator();
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
                remote_static,
                proven,
            },
            handshake_hash,
        })
    }
}

/// A finished handshake: frames in both directions.
#[derive(Debug)]
pub struct Session {
    sealer: Sealer,
    opener: Opener,
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

    /// The other side's static key, once it is proven: see
    /// [`Opener::remote_static`].
    #[must_use]
    pub const fn remote_static(&self) -> Option<[u8; KEY_LEN]> {
        self.opener.remote_static()
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
    remote_static: [u8; KEY_LEN],
    proven: bool,
}

impl std::fmt::Debug for Opener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Opener")
            .field("nonce", &self.nonce)
            .field("proven", &self.proven)
            .finish_non_exhaustive()
    }
}

impl Opener {
    /// The other side's static key, or `None` until it is proven.
    ///
    /// On the client it is the pinned host key, known from the start. On the
    /// host it is the device key, and it is `None` until one transport frame
    /// from the device has opened: IK message 1 can be replayed by anyone who
    /// saw it, and a replayer can finish the host's half of the handshake but
    /// never produce a frame. Bind a token, a device row or anything else to
    /// the key only through this call.
    #[must_use]
    pub const fn remote_static(&self) -> Option<[u8; KEY_LEN]> {
        if self.proven {
            Some(self.remote_static)
        } else {
            None
        }
    }

    /// Decrypt one Noise message into one frame.
    ///
    /// A message that does not decrypt leaves the nonce where it was, so the
    /// session is dead ([`NoiseError::Decrypt`] is fatal). A message that
    /// decrypts but holds an unknown opcode, or a reserved one that version 1
    /// does not use ([`Opcode::in_use`](crate::opcode::Opcode::in_use)), has
    /// used its nonce and is dropped with
    /// [`HeaderError::UnknownOpcode`]; the session goes on.
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
        // It authenticated under this session's keys, so the peer holds the
        // static key the handshake named.
        self.proven = true;
        let frame = Frame::decode(&plain[..len]).map_err(NoiseError::Frame)?;
        if !frame.header.opcode.in_use() {
            // No capability names a reserved opcode yet (the binary lane),
            // so none is accepted.
            return Err(NoiseError::Frame(HeaderError::UnknownOpcode(
                frame.header.opcode.as_u8(),
            )));
        }
        Ok(frame)
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

    /// F2: a low-order device static is refused. A real peer cannot send one
    /// (its public key comes from a clamped private key), so the guard the
    /// responder runs after message 1 is exercised directly.
    #[test]
    fn the_responder_refuses_a_low_order_device_static() {
        let mut one = [0u8; KEY_LEN];
        one[0] = 1;
        for low in [[0u8; KEY_LEN], one] {
            assert_eq!(refuse_low_order(Some(low)), Err(NoiseError::LowOrderKey));
        }
        let genuine = generate_keypair().unwrap();
        assert_eq!(refuse_low_order(Some(genuine.public)), Ok(()));
        assert_eq!(refuse_low_order(None), Ok(()));
        assert!(NoiseError::LowOrderKey.is_fatal());
    }

    /// A refused handshake stays refused.
    #[test]
    fn a_refused_handshake_refuses_every_later_call() {
        let host = generate_keypair().unwrap();
        let id = HostId::parse(HOST).unwrap();
        let mut r = responder(&host.private, CarrierKind::Lan, &id).unwrap();
        r.refused = true;
        assert!(matches!(
            r.read_message(&[0; 96]),
            Err(NoiseError::State(_))
        ));
        assert!(matches!(r.write_message(), Err(NoiseError::State(_))));
        assert!(matches!(r.into_session(), Err(NoiseError::State(_))));
    }
}
