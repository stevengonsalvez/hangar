//! The Noise IK handshake and transport, through the public API only (R1-01).
//!
//! What the daemon's listener (R1-06), the desktop client (R1-09) and the
//! phone crate build on:
//!
//! 1. a handshake between a pinned host key and a device key yields two
//!    sessions that carry frames both ways, and the host is handed the device
//!    key only once a frame from the device opens (message 1 can be replayed);
//! 2. the three G5 cases (wrong carrier, unpinned key, wrong host id) fail at
//!    message 1, before the host writes anything;
//! 3. a tampered, replayed or cross-session message does not decrypt;
//! 4. a 16 MiB logical Rpc message survives fragmenting, sealing, opening and
//!    reassembly, and one byte more is refused.

use ainb_hangar_noise::noise::KEY_LEN;
use ainb_hangar_noise::opcode::Opcode;
use ainb_hangar_noise::reassembly::{MAX_FRAME_PAYLOAD, TAG_LEN};
use ainb_hangar_noise::{
    Frame, Handshake, Keypair, MAX_NOISE_MESSAGE, MAX_REASSEMBLED, NoiseError, PrologueError,
    Reassembler, ReassemblyError, Session, generate_keypair, initiator, lsp_body, lsp_encode,
    public_key, responder, rpc_frames,
};
use ainb_hangar_proto::hosts::{CarrierKind, HostId, HostIdError};

const HOST: &str = "01K5A0000000000000000ABCDE";
const OTHER_HOST: &str = "01K5A0000000000000000ABCDF";

fn host_id(s: &str) -> HostId {
    HostId::parse(s).unwrap()
}

struct Keys {
    host: Keypair,
    device: Keypair,
}

fn keys() -> Keys {
    Keys {
        host: generate_keypair().unwrap(),
        device: generate_keypair().unwrap(),
    }
}

/// Run both messages; returns (client, host).
fn handshake(mut client: Handshake, mut host: Handshake) -> Result<(Session, Session), NoiseError> {
    let msg1 = client.write_message()?;
    host.read_message(&msg1)?;
    let msg2 = host.write_message()?;
    client.read_message(&msg2)?;
    Ok((client.into_session()?, host.into_session()?))
}

fn connected(k: &Keys) -> (Session, Session) {
    let id = host_id(HOST);
    handshake(
        initiator(&k.device.private, &k.host.public, CarrierKind::Tailnet, &id).unwrap(),
        responder(&k.host.private, CarrierKind::Tailnet, &id).unwrap(),
    )
    .unwrap()
}

#[test]
fn ik_yields_two_sessions_that_carry_frames_both_ways() {
    let k = keys();
    let id = host_id(HOST);
    let mut client =
        initiator(&k.device.private, &k.host.public, CarrierKind::Tailnet, &id).unwrap();
    let mut host = responder(&k.host.private, CarrierKind::Tailnet, &id).unwrap();

    let msg1 = client.write_message().unwrap();
    assert!(client.write_message().is_err(), "msg2 is the host's turn");
    host.read_message(&msg1).unwrap();
    let msg2 = host.write_message().unwrap();
    client.read_message(&msg2).unwrap();
    assert!(client.is_finished() && host.is_finished());

    let mut client = client.into_session().unwrap();
    let mut host = host.into_session().unwrap();
    assert_eq!(
        client.remote_static(),
        Some(k.host.public),
        "the pinned key"
    );
    assert_eq!(
        host.remote_static(),
        None,
        "no device key before a frame from the device opens"
    );
    assert_eq!(client.handshake_hash(), host.handshake_hash());
    assert!(!client.handshake_hash().is_empty());

    let hello = rpc_frames(&lsp_encode(
        br#"{"jsonrpc":"2.0","id":1,"method":"auth/hello"}"#,
    ));
    assert_eq!(hello.len(), 1);
    let sealed = client.encrypt_frame(&hello[0]).unwrap();
    assert_eq!(sealed.len(), hello[0].encode().len() + TAG_LEN);
    assert_eq!(host.decrypt_frame(&sealed).unwrap(), hello[0]);
    assert_eq!(host.remote_static(), Some(k.device.public));

    for seq in 0..3 {
        let ping = Frame::control(Opcode::Ping, seq, Vec::new());
        let got = host.decrypt_frame(&client.encrypt_frame(&ping).unwrap()).unwrap();
        let pong = Frame::control(Opcode::Pong, got.header.seq, Vec::new());
        let back = client.decrypt_frame(&host.encrypt_frame(&pong).unwrap()).unwrap();
        assert_eq!(back.header.opcode, Opcode::Pong);
        assert_eq!(back.header.seq, seq, "the Pong echoes the Ping's seq");
    }
}

/// G5: a client that dialed another carrier, pinned a key the host does not
/// hold, or dialed another host id fails at message 1. The host never writes
/// message 2, so it reads no frame from that socket.
#[test]
fn the_three_g5_cases_fail_at_message_one() {
    let k = keys();
    let impostor = generate_keypair().unwrap();
    let id = host_id(HOST);
    let other = host_id(OTHER_HOST);
    let cases: [(&str, [u8; KEY_LEN], CarrierKind, &HostId); 3] = [
        ("wrong carrier", k.host.public, CarrierKind::SshL, &id),
        ("unpinned key", impostor.public, CarrierKind::Tailnet, &id),
        ("wrong host id", k.host.public, CarrierKind::Tailnet, &other),
    ];
    for (name, pinned, carrier, dialed) in cases {
        let mut client = initiator(&k.device.private, &pinned, carrier, dialed).unwrap();
        let mut host = responder(&k.host.private, CarrierKind::Tailnet, &id).unwrap();
        let msg1 = client.write_message().unwrap();
        assert_eq!(host.read_message(&msg1), Err(NoiseError::Decrypt), "{name}");
        assert!(!host.is_finished(), "{name}");
        assert!(
            host.into_session().is_err(),
            "{name}: no session from a failed handshake"
        );
    }
}

#[test]
fn a_peer_is_never_local() {
    let k = keys();
    for result in [
        initiator(
            &k.device.private,
            &k.host.public,
            CarrierKind::Lan,
            &HostId::local(),
        ),
        responder(&k.host.private, CarrierKind::Lan, &HostId::local()),
    ] {
        assert_eq!(
            result.unwrap_err(),
            NoiseError::Prologue(PrologueError::Host(HostIdError::LocalNotAllowed))
        );
    }
    let id = host_id(HOST);
    assert_eq!(
        responder(&k.host.private, CarrierKind::Unknown, &id).unwrap_err(),
        NoiseError::Prologue(PrologueError::UnknownCarrier)
    );
}

#[test]
fn a_tampered_replayed_or_foreign_message_does_not_decrypt() {
    let k = keys();
    let (mut client, mut host) = connected(&k);
    let ping = Frame::control(Opcode::Ping, 1, b"x".to_vec());

    let mut tampered = client.encrypt_frame(&ping).unwrap();
    tampered[3] ^= 1;
    let err = host.decrypt_frame(&tampered).unwrap_err();
    assert_eq!(err, NoiseError::Decrypt);
    assert!(err.is_fatal());

    let (mut client, mut host) = connected(&k);
    let first = client.encrypt_frame(&ping).unwrap();
    assert_eq!(host.decrypt_frame(&first).unwrap(), ping);
    assert_eq!(
        host.decrypt_frame(&first),
        Err(NoiseError::Decrypt),
        "a replay reuses a spent nonce"
    );

    let (mut client, _) = connected(&k);
    let (_, mut other_host) = connected(&k);
    let sealed = client.encrypt_frame(&ping).unwrap();
    assert_eq!(
        other_host.decrypt_frame(&sealed),
        Err(NoiseError::Decrypt),
        "every session has its own keys, even between the same two static keys"
    );
    assert_eq!(host_short(&mut other_host), Err(NoiseError::Decrypt));
}

fn host_short(session: &mut Session) -> Result<Frame, NoiseError> {
    session.decrypt_frame(&[0u8; TAG_LEN - 1])
}

#[test]
fn a_frame_larger_than_one_noise_message_is_refused() {
    let k = keys();
    let (mut client, mut host) = connected(&k);
    let fits = Frame::control(Opcode::Rpc, 0, vec![1; MAX_FRAME_PAYLOAD]);
    let sealed = client.encrypt_frame(&fits).unwrap();
    assert_eq!(sealed.len(), MAX_NOISE_MESSAGE);
    assert_eq!(host.decrypt_frame(&sealed).unwrap(), fits);
    let over = Frame::control(Opcode::Rpc, 0, vec![1; MAX_FRAME_PAYLOAD + 1]);
    assert_eq!(
        client.encrypt_frame(&over),
        Err(NoiseError::TooLarge(MAX_NOISE_MESSAGE + 1))
    );
    assert_eq!(
        host.decrypt_frame(&vec![0; MAX_NOISE_MESSAGE + 1]),
        Err(NoiseError::TooLarge(MAX_NOISE_MESSAGE + 1))
    );
}

/// A recorded message 1 replayed to the host completes the host's half of
/// the handshake, but the replayer holds no key to send a frame with, so the
/// host is never handed the device key and never has a frame to act on.
#[test]
fn a_replayed_message_one_proves_nothing() {
    let k = keys();
    let id = host_id(HOST);
    let mut genuine =
        initiator(&k.device.private, &k.host.public, CarrierKind::Tailnet, &id).unwrap();
    let recorded = genuine.write_message().unwrap();

    let mut host = responder(&k.host.private, CarrierKind::Tailnet, &id).unwrap();
    host.read_message(&recorded).unwrap();
    let _answer = host.write_message().unwrap();
    let mut host = host.into_session().unwrap();
    assert_eq!(host.remote_static(), None);
    // The replayer guesses at frames; none opens, and the key stays unproven.
    for guess in [vec![0u8; 64], vec![0xff; 32], recorded.clone()] {
        assert_eq!(host.decrypt_frame(&guess), Err(NoiseError::Decrypt));
    }
    assert_eq!(host.remote_static(), None);
}

/// A reserved opcode (R2's binary lane) decrypts but is dropped as unknown
/// while no capability names it; the session goes on.
#[test]
fn a_reserved_opcode_is_dropped_and_the_session_goes_on() {
    let k = keys();
    let (mut client, mut host) = connected(&k);
    for reserved in Opcode::ALL.into_iter().filter(|op| !op.in_use()) {
        let frame = Frame::control(reserved, 0, b"x".to_vec());
        let err = host.decrypt_frame(&client.encrypt_frame(&frame).unwrap()).unwrap_err();
        assert_eq!(
            err,
            NoiseError::Frame(ainb_hangar_noise::frame::HeaderError::UnknownOpcode(
                reserved.as_u8()
            )),
            "{reserved:?}"
        );
        assert!(!err.is_fatal());
    }
    let ping = Frame::control(Opcode::Ping, 1, Vec::new());
    assert_eq!(
        host.decrypt_frame(&client.encrypt_frame(&ping).unwrap()),
        Ok(ping)
    );
}

/// For R1-07's redeem check: the known low-order X25519 points (including
/// the non-canonical encodings of 0 and 1) are flagged, real keys are not.
#[test]
fn low_order_device_keys_are_recognised() {
    fn hex(s: &str) -> [u8; KEY_LEN] {
        std::array::from_fn(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
    }
    let mut p_minus_1 = [0xff; KEY_LEN];
    p_minus_1[0] = 0xec;
    p_minus_1[31] = 0x7f;
    let mut p = p_minus_1;
    p[0] = 0xed;
    let mut p_plus_1 = p_minus_1;
    p_plus_1[0] = 0xee;
    let mut one = [0u8; KEY_LEN];
    one[0] = 1;
    let low = [
        [0u8; KEY_LEN],
        one,
        hex("e0eb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b800"),
        hex("5f9c95bca3508c24b1d0b1559c83ef5b04445cc4581c8e86d8224eddd09f1157"),
        p_minus_1,
        p,
        p_plus_1,
    ];
    for point in low {
        assert!(ainb_hangar_noise::is_low_order(&point), "{point:02x?}");
    }
    for _ in 0..16 {
        assert!(!ainb_hangar_noise::is_low_order(
            &generate_keypair().unwrap().public
        ));
    }
}

/// The unix leg's 16 MiB cap holds end to end on the peer leg.
#[test]
fn a_16_mib_rpc_message_round_trips_and_one_byte_more_is_refused() {
    let k = keys();
    let (client, host) = connected(&k);
    let (mut sealer, _) = client.split();
    let (_, mut opener) = host.split();

    let header_len = lsp_encode(&[]).len() + MAX_REASSEMBLED.to_string().len() - 1;
    let body: Vec<u8> = (0..MAX_REASSEMBLED - header_len)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    let message = lsp_encode(&body);
    assert_eq!(message.len(), MAX_REASSEMBLED);

    let frames = rpc_frames(&message);
    assert_eq!(frames.len(), MAX_REASSEMBLED.div_ceil(MAX_FRAME_PAYLOAD));
    let mut reassembler = Reassembler::new();
    let mut whole = None;
    for frame in &frames {
        let sealed = sealer.seal(frame).unwrap();
        assert!(sealed.len() <= MAX_NOISE_MESSAGE);
        let opened = opener.open(&sealed).unwrap();
        whole = reassembler.push(&opened).unwrap();
    }
    let whole = whole.expect("the last fragment is final");
    assert_eq!(lsp_body(&whole).unwrap(), &body[..]);

    let over = vec![0u8; MAX_REASSEMBLED + 1];
    let mut reassembler = Reassembler::new();
    let result: Result<Vec<_>, _> = rpc_frames(&over)
        .iter()
        .map(|frame| reassembler.push(&opener.open(&sealer.seal(frame).unwrap()).unwrap()))
        .collect();
    assert_eq!(
        result.unwrap_err(),
        ReassemblyError::TooLarge {
            cap: MAX_REASSEMBLED
        }
    );
}

/// The halves move to a reader task and a writer task.
#[test]
fn the_halves_run_on_their_own_threads() {
    let k = keys();
    let (client, host) = connected(&k);
    let (mut client_tx, mut client_rx) = client.split();
    let (mut host_tx, mut host_rx) = host.split();
    let up = std::thread::spawn(move || {
        (0..100)
            .map(|seq| client_tx.seal(&Frame::control(Opcode::Ping, seq, Vec::new())).unwrap())
            .collect::<Vec<_>>()
    });
    let down = std::thread::spawn(move || {
        (0..100)
            .map(|seq| host_tx.seal(&Frame::control(Opcode::Pong, seq, Vec::new())).unwrap())
            .collect::<Vec<_>>()
    });
    for (seq, sealed) in up.join().unwrap().iter().enumerate() {
        assert_eq!(host_rx.open(sealed).unwrap().header.seq, seq as u64);
    }
    for (seq, sealed) in down.join().unwrap().iter().enumerate() {
        assert_eq!(client_rx.open(sealed).unwrap().header.seq, seq as u64);
    }
}

#[test]
fn a_key_pair_is_fresh_and_its_private_half_never_prints() {
    let a = generate_keypair().unwrap();
    let b = generate_keypair().unwrap();
    assert_ne!(a.private, b.private);
    assert_eq!(public_key(&a.private).unwrap(), a.public);
    let shown = format!("{a:?}");
    assert!(shown.contains("redacted"));
    let private_hex = a.private.iter().fold(String::new(), |mut out, b| {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
        out
    });
    assert!(!shown.contains(&format!("{:?}", a.private)));
    assert!(!shown.contains(&private_hex));
}

/// F2 end to end: snow forges message 1 with a static public key of 1 when
/// its resolver reports one (the review's proof), and the host refuses that
/// message with `LowOrderKey` and answers nothing.
#[test]
fn a_forged_low_order_static_is_refused_at_message_one() {
    use snow::resolvers::{CryptoResolver, DefaultResolver};
    use snow::types::{Cipher, Dh, Hash, Random};

    /// X25519 that forges exactly one key: once `set` gives it the target
    /// private key, it reports the low-order public key 1 and the all-zero
    /// DH output that point gives. Any other key (the ephemeral, which snow
    /// makes through `generate`) stays honest.
    struct ForgedDh {
        inner: Box<dyn Dh>,
        target: [u8; KEY_LEN],
        forged: bool,
        public: [u8; KEY_LEN],
    }
    impl Dh for ForgedDh {
        fn name(&self) -> &'static str {
            self.inner.name()
        }
        fn pub_len(&self) -> usize {
            self.inner.pub_len()
        }
        fn priv_len(&self) -> usize {
            self.inner.priv_len()
        }
        fn set(&mut self, privkey: &[u8]) {
            self.forged = privkey == self.target;
            self.inner.set(privkey);
        }
        fn generate(&mut self, rng: &mut dyn Random) {
            self.forged = false;
            self.inner.generate(rng);
        }
        fn pubkey(&self) -> &[u8] {
            if self.forged {
                &self.public
            } else {
                self.inner.pubkey()
            }
        }
        fn privkey(&self) -> &[u8] {
            self.inner.privkey()
        }
        // With a public key of 1, the host's `ss` is DH(host, 1) = 0. The
        // forger matches it, so message 1 authenticates and only the key
        // check can stop it.
        fn dh(&self, pubkey: &[u8], out: &mut [u8]) -> Result<(), snow::Error> {
            if self.forged {
                out[..KEY_LEN].fill(0);
                Ok(())
            } else {
                self.inner.dh(pubkey, out)
            }
        }
    }

    /// The default resolver, except every X25519 forges the target key.
    struct ForgedResolver([u8; KEY_LEN]);
    impl CryptoResolver for ForgedResolver {
        fn resolve_rng(&self) -> Option<Box<dyn Random>> {
            DefaultResolver.resolve_rng()
        }
        fn resolve_dh(&self, choice: &snow::params::DHChoice) -> Option<Box<dyn Dh>> {
            let mut public = [0u8; KEY_LEN];
            public[0] = 1;
            Some(Box::new(ForgedDh {
                inner: DefaultResolver.resolve_dh(choice)?,
                target: self.0,
                forged: false,
                public,
            }))
        }
        fn resolve_hash(&self, choice: &snow::params::HashChoice) -> Option<Box<dyn Hash>> {
            DefaultResolver.resolve_hash(choice)
        }
        fn resolve_cipher(&self, choice: &snow::params::CipherChoice) -> Option<Box<dyn Cipher>> {
            DefaultResolver.resolve_cipher(choice)
        }
    }

    let k = keys();
    let id = host_id(HOST);
    let prologue = ainb_hangar_noise::prologue(CarrierKind::Tailnet, &id).unwrap();
    let mut forger = snow::Builder::with_resolver(
        ainb_hangar_noise::NOISE_PATTERN.parse().unwrap(),
        Box::new(ForgedResolver(k.device.private)),
    )
    .local_private_key(&k.device.private)
    .remote_public_key(&k.host.public)
    .prologue(&prologue)
    .build_initiator()
    .unwrap();
    let mut msg1 = vec![0u8; 1024];
    let len = forger.write_message(&[], &mut msg1).unwrap();

    let mut host = responder(&k.host.private, CarrierKind::Tailnet, &id).unwrap();
    assert_eq!(
        host.read_message(&msg1[..len]),
        Err(NoiseError::LowOrderKey)
    );
    assert!(host.write_message().is_err(), "the host answers nothing");
    assert!(host.into_session().is_err());
}
