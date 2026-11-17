use tokio::sync::mpsc::error::TryRecvError;

use super::*;
use crate::pulse::transport::pulse_channels;

fn address(text: &str) -> SocketAddr {
    text.parse().unwrap()
}

#[test]
fn prefers_the_first_ipv4_address() {
    let picked = pick_address([
        address("[::1]:7777"),
        address("10.0.0.2:7777"),
        address("10.0.0.3:7777"),
    ]);

    assert_eq!(picked, Some(address("10.0.0.2:7777")));
}

#[test]
fn falls_back_to_the_first_ipv6_address() {
    let picked = pick_address([address("[::1]:7777"), address("[::2]:7777")]);

    assert_eq!(picked, Some(address("[::1]:7777")));
    assert_eq!(pick_address([]), None);
}

#[test]
fn a_full_queue_holds_reliable_and_drops_unreliable() {
    let (tx, mut rx) = mpsc::channel(2);
    let mut backlog = Backlog::with_caps(8, MAX_HELD_BYTES);

    backlog.deliver(&tx, vec![1], true).unwrap();
    backlog.deliver(&tx, vec![2], true).unwrap();
    // The queue is full now: the reliable message is held, the unreliable one dropped.
    backlog.deliver(&tx, vec![3], true).unwrap();
    backlog.deliver(&tx, vec![4], false).unwrap();

    assert_eq!(rx.try_recv(), Ok(vec![1]));
    assert_eq!(rx.try_recv(), Ok(vec![2]));
    backlog.flush(&tx);
    assert_eq!(rx.try_recv(), Ok(vec![3]));
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
}

#[test]
fn a_reliable_message_never_overtakes_a_held_one() {
    let (tx, mut rx) = mpsc::channel(1);
    let mut backlog = Backlog::with_caps(8, MAX_HELD_BYTES);

    backlog.deliver(&tx, vec![1], true).unwrap();
    backlog.deliver(&tx, vec![2], true).unwrap();
    assert_eq!(rx.try_recv(), Ok(vec![1]));

    // There is room again, but the second message is still held, so the third queues behind it.
    backlog.deliver(&tx, vec![3], true).unwrap();
    backlog.flush(&tx);
    assert_eq!(rx.try_recv(), Ok(vec![2]));
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    backlog.flush(&tx);
    assert_eq!(rx.try_recv(), Ok(vec![3]));
}

#[test]
fn holding_past_the_message_cap_is_an_error() {
    let (tx, _rx) = mpsc::channel(1);
    let mut backlog = Backlog::with_caps(1, MAX_HELD_BYTES);

    backlog.deliver(&tx, vec![1], true).unwrap();
    backlog.deliver(&tx, vec![2], true).unwrap();

    assert_eq!(backlog.deliver(&tx, vec![3], true), Err(BacklogFull));
}

#[test]
fn holding_past_the_byte_cap_is_an_error() {
    let (tx, mut rx) = mpsc::channel(1);
    let mut backlog = Backlog::with_caps(8, 4);

    backlog.deliver(&tx, vec![1], true).unwrap();
    // The queue is full now: three bytes are held, and two more would be over the four.
    backlog.deliver(&tx, vec![2; 3], true).unwrap();
    assert_eq!(backlog.deliver(&tx, vec![3; 2], true), Err(BacklogFull));

    // Handed over, a held message frees its bytes: with the queue full again, all four can be
    // held at once.
    assert_eq!(rx.try_recv(), Ok(vec![1]));
    backlog.flush(&tx);
    assert_eq!(backlog.deliver(&tx, vec![4; 4], true), Ok(()));
    assert_eq!(rx.try_recv(), Ok(vec![2; 3]));
}

/// A host on a loopback socket of its own, with the driver's settings.
fn loopback_host() -> enet::Host<std::net::UdpSocket> {
    enet::Host::new(
        std::net::UdpSocket::bind("127.0.0.1:0").unwrap(),
        enet::HostSettings {
            peer_limit: 1,
            channel_limit: CHANNEL_COUNT,
            ..Default::default()
        },
    )
    .unwrap()
}

/// A driver that gives up on a connected peer, its inbound backlog over a cap or ENet's service
/// failing, tells the server at once. Otherwise the server holds the peer until its own timeout
/// notices, which takes ENet at least 5 s. This covers a connected peer only: before VERIFY_CONNECT
/// the notice cannot reach the server (see `abandon`).
#[test]
fn abandoning_a_connection_tells_the_server_at_once() {
    let (mut client, peer, mut server) = connected_pair();

    let (mut link, mut channels) = pulse_channels(1);
    abandon(&mut client, peer, &mut channels, "gave up".to_owned());

    let deadline = Instant::now() + Duration::from_secs(1);
    let reason = loop {
        assert!(Instant::now() < deadline, "the server was not told");
        if let Ok(Some(enet::Event::Disconnect { data, .. })) = server.service() {
            break data;
        }
        thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(PulseDisconnect::from_code(reason), PulseDisconnect::None);
    assert!(matches!(
        link.status.try_recv(),
        Ok(PulseStatus::Failed(message)) if message == "gave up"
    ));
}

/// Two loopback hosts, connected: the client and its peer id, and the server.
fn connected_pair() -> (
    enet::Host<std::net::UdpSocket>,
    enet::PeerID,
    enet::Host<std::net::UdpSocket>,
) {
    let mut server = loopback_host();
    let mut client = loopback_host();
    let address = server.socket().local_addr().unwrap();
    let peer = client.connect(address, CHANNEL_COUNT, 0).unwrap().id();
    let deadline = Instant::now() + Duration::from_secs(5);
    let (mut client_up, mut server_up) = (false, false);
    while !(client_up && server_up) {
        assert!(Instant::now() < deadline, "the connection never came up");
        client_up |= matches!(client.service(), Ok(Some(enet::Event::Connect { .. })));
        server_up |= matches!(server.service(), Ok(Some(enet::Event::Connect { .. })));
        thread::sleep(Duration::from_millis(1));
    }
    (client, peer, server)
}

/// The gate's signal on a live peer: a reliable message and a probe are unacknowledged until the
/// server's acknowledgements arrive, and then nothing is, so the gate does not hold forever.
#[test]
fn a_live_peer_reports_reliable_traffic_unacknowledged_until_acknowledged() {
    let (mut client, peer, mut server) = connected_pair();
    // Settle the connect handshake's own acknowledgements.
    let settle = |client: &mut enet::Host<_>, server: &mut enet::Host<_>| {
        let deadline = Instant::now() + Duration::from_secs(2);
        while client.peer(peer).has_unacknowledged_reliable() {
            assert!(Instant::now() < deadline, "never acknowledged");
            let _ = server.service();
            let _ = client.service();
            thread::sleep(Duration::from_millis(1));
        }
    };
    settle(&mut client, &mut server);

    let (channel, packet) = packet_for_frame(&frame(FrameKind::Emote, 1));
    client.peer_mut(peer).send(channel, &packet).unwrap();
    client.peer_mut(peer).ping();
    client.flush();
    assert!(client.peer(peer).has_unacknowledged_reliable());

    settle(&mut client, &mut server);
    assert!(!client.peer(peer).has_unacknowledged_reliable());
}

#[test]
fn transient_socket_errors_leave_the_session_to_enets_timeout() {
    for kind in [
        ErrorKind::WouldBlock,
        ErrorKind::ConnectionReset,
        ErrorKind::ConnectionRefused,
        ErrorKind::NetworkUnreachable,
        ErrorKind::HostUnreachable,
        ErrorKind::NetworkDown,
    ] {
        assert!(is_transient(&io::Error::from(kind)), "{kind:?}");
    }
    if let Some(code) = ENOBUFS {
        assert!(is_transient(&io::Error::from_raw_os_error(code)));
    }
}

#[test]
fn other_socket_errors_still_end_the_session() {
    for kind in [
        ErrorKind::PermissionDenied,
        ErrorKind::InvalidInput,
        ErrorKind::AddrNotAvailable,
        ErrorKind::Other,
    ] {
        assert!(!is_transient(&io::Error::from(kind)), "{kind:?}");
    }
}

fn frame(kind: FrameKind, tag: u8) -> PulseFrame {
    PulseFrame {
        bytes: vec![tag],
        reliability: if kind == FrameKind::Input {
            PulseReliability::UnreliableSequenced
        } else {
            PulseReliability::Reliable
        },
        kind,
    }
}

fn tags(frames: &[PulseFrame]) -> Vec<(FrameKind, u8)> {
    frames.iter().map(|f| (f.kind, f.bytes[0])).collect()
}

#[test]
fn an_acknowledged_uplink_lets_everything_through() {
    let mut gate = UplinkGate::default();
    let now = Instant::now();
    // Briefly unacknowledged, as after any send, is no stall.
    gate.observe(now, true);
    let out = gate.admit(
        vec![frame(FrameKind::Emote, 1), frame(FrameKind::Input, 2)],
        now + STALL_AFTER / 2,
    );

    assert_eq!(tags(&out), [(FrameKind::Emote, 1), (FrameKind::Input, 2)]);
}

#[test]
fn a_stalled_uplink_holds_only_the_latest_input_until_it_recovers() {
    let mut gate = UplinkGate::default();
    let start = Instant::now();
    gate.observe(start, true);
    let stalled = start + STALL_AFTER;

    for tag in 0..30 {
        gate.observe(stalled, true);
        assert!(gate
            .admit(vec![frame(FrameKind::Input, tag)], stalled)
            .is_empty());
    }
    // A handshake, a resync or an announcement is never held.
    let control = gate.admit(vec![frame(FrameKind::Control, 99)], stalled);
    assert_eq!(tags(&control), [(FrameKind::Control, 99)]);

    gate.observe(stalled, false);
    let out = gate.admit(Vec::new(), stalled);
    assert_eq!(tags(&out), [(FrameKind::Input, 29)]);
}

#[test]
fn unconfirmed_teleports_and_emotes_stay_within_half_the_servers_burst() {
    let mut gate = UplinkGate::default();
    let now = Instant::now();
    gate.observe(now, true);

    let mut let_out = 0;
    for tag in 0..20 {
        let kind = if tag % 2 == 0 {
            FrameKind::Emote
        } else {
            FrameKind::Teleport
        };
        let out = gate.admit(vec![frame(kind, tag)], now);
        let_out += out.len();
        // Inputs queue behind a held start, which carries an older pose.
        assert!(
            gate.admit(vec![frame(FrameKind::Input, 100 + tag)], now)
                .len()
                <= 1
        );
    }
    assert_eq!(let_out, MAX_UNCONFIRMED_DISCRETE);
    const _: () = assert!(MAX_UNCONFIRMED_DISCRETE * 2 <= 16);

    // Everything acknowledged: the latest of each kind goes, the input after them.
    gate.observe(now, false);
    let out = gate.admit(Vec::new(), now);
    assert_eq!(
        tags(&out),
        [
            (FrameKind::Emote, 18),
            (FrameKind::Teleport, 19),
            (FrameKind::Input, 119)
        ]
    );
}

#[test]
fn the_snapshot_reports_what_the_gate_holds() {
    let mut gate = UplinkGate::default();
    let start = Instant::now();
    gate.observe(start, true);
    let stalled = start + STALL_AFTER;
    gate.admit(
        vec![frame(FrameKind::Teleport, 1), frame(FrameKind::Input, 2)],
        stalled,
    );

    assert_eq!(
        gate.snapshot(stalled),
        UplinkSnapshot {
            stalled: true,
            unconfirmed_discrete: 0,
            held_teleport: true,
            held_emote: false,
            held_input: true,
        }
    );
    gate.observe(stalled, false);
    gate.admit(Vec::new(), stalled);
    assert_eq!(
        gate.snapshot(stalled),
        UplinkSnapshot {
            unconfirmed_discrete: 1,
            ..Default::default()
        }
    );
}

#[test]
fn the_uplink_is_probed_only_when_nothing_reliable_is_in_flight() {
    let mut gate = UplinkGate::default();
    let now = Instant::now();

    assert!(gate.wants_probe(now));
    assert!(!gate.wants_probe(now + PROBE_INTERVAL / 2));
    gate.observe(now + PROBE_INTERVAL, true);
    assert!(!gate.wants_probe(now + PROBE_INTERVAL));
    gate.observe(now + PROBE_INTERVAL * 2, false);
    assert!(gate.wants_probe(now + PROBE_INTERVAL * 2));
}
