# Oracle derivations (hand-computed, RFC 1982 serial arithmetic)

These goldens are the **independent check** for M3 metric derivation (ADR-0007). Every
load-bearing value below is derived by hand from the fixture's segments — it is **not** a
snapshot of program output. If `tcp-visr metrics <fixture> --conn 0` disagrees with a value
here, the trace below is the authority and the code is the suspect. Regenerating a golden
(`tests/metrics.rs` is the byte-match check) requires re-deriving the numbers by hand and
reviewing the diff.

`o2r` = client(10.0.0.1:1234) -> server(10.0.0.2:80). `seq_end = seq + payload + SYN + FIN`.
in-flight = `serial_diff(snd_nxt[d], acked[d])` (clamped to 0 if acked is ahead). An ACK
advances (and can pair RTT) only once the acked direction has a tracked send.

## seq_wrap.pcap (--conn 0)

- seg1 o2r seq=2^32-101 len=50 ts=1us: acked[o2r]=2^32-101 (first o2r seq), snd_nxt=2^32-51,
  in_flight = serial_diff(2^32-51, 2^32-101) = 50. ACK=1 but r2o has no send -> no rtt.
- seg2 o2r seq=200 len=50 ts=2us: snd_nxt=250 (forward wrap), in_flight=serial_diff(250,2^32-101)=351.
- seg3 r2o seq=1 ack=300 len=10 ts=3us: r2o in_flight=serial_diff(11,1)=10; ack=300 advances
  acked[o2r], covers o2r sends (seq_end 2^32-51, 250 <= 300) -> rtt pairs oldest (ts1=1us):
  rtt = 3us-1us = 2000ns.

## metrics_basic.pcap (--conn 0) — exercises a piggybacked ACK; all four samples enumerated

- seg1 o2r SYN seq=1000 ts=1us: acked[o2r]=1000, snd_nxt[o2r]=1001 (SYN phantom),
  in_flight=serial_diff(1001,1000)=1. No ACK flag -> no rtt. pending_rtt[o2r]=[(1001,1us)].
  throughput 0 (no payload). => {dir o2r, in_flight 1, tput 0, rtt null}
- seg2 r2o SYN-ACK seq=5000 ack=1001 ts=2us: acked[r2o]=5000, snd_nxt[r2o]=5001 (SYN phantom),
  in_flight[r2o]=serial_diff(5001,5000)=1. ack=1001 advances acked[o2r] (1000->1001), pops
  pending_rtt[o2r]=[(1001,1us)] -> rtt=2us-1us=1000ns (handshake RTT). pending_rtt[r2o]=[(5001,2us)].
  => {dir r2o, in_flight 1, tput 0, rtt 1000}
- seg3 o2r data seq=1001 len=100 ack=5001 ts=3us: snd_nxt[o2r]=1101,
  in_flight[o2r]=serial_diff(1101,1001)=100. Data (not retransmit) -> pending_rtt[o2r]=[(1101,3us)].
  ack=5001 advances acked[r2o] (5000->5001), pops pending_rtt[r2o]=[(5001,2us)] -> rtt=3us-2us=1000ns
  (SYN-ACK round trip). throughput[o2r]: 100 bytes in (3us-1s,3us] -> 8*100*1e9/1e9 = 800 bps.
  => {dir o2r, in_flight 100, tput 800, rtt 1000}
- seg4 r2o pure-ACK seq=5001 ack=1101 len=0 ts=4us: snd_nxt[r2o]=5001; acked[r2o] already 5001
  (advanced by seg3) -> in_flight[r2o]=serial_diff(5001,5001)=0. ack=1101 advances acked[o2r]
  (1001->1101), pops pending_rtt[o2r]=[(1101,3us)] -> rtt=4us-3us=1000ns (data RTT). tput 0.
  => {dir r2o, in_flight 0, tput 0, rtt 1000}

So metrics_basic yields rtts [null, 1000, 1000, 1000] and in_flight [1, 1, 100, 0].

## metrics_retransmit.pcap (--conn 0)

Both segments are o2r; no reverse ACK, so no RTT anywhere.

- seg1 o2r seq=100 len=100 ts=1us: acked[o2r]=100, snd_nxt=200, in_flight=100, frontier=200.
- seg2 o2r seq=100 len=100 ts=3_001_000ns: seq=100 < frontier 200, gap=3_001_000-1_000=3_000_000
  >= reorder_window (3ms) -> retransmit=true; clears pending RTT. in_flight=serial_diff(200,100)=100.

## metrics_ooo.pcap (--conn 0)

Both o2r; no reverse ACK -> no rtt.

- seg1 o2r seq=200 len=100 ts=1us: in_flight=100, frontier=300.
- seg2 o2r seq=100 len=100 ts=1_001ns: seq=100 < frontier 300, gap=1ns... actually gap=1_001-1_000=1
  (1us in file, microsecond ts) < 3ms -> out_of_order=true. in_flight=serial_diff(300,200)=100.

## metrics_sack.pcap (--conn 0) — seg2 also acks seg1, so it carries a data RTT

- seg1 o2r seq=100 len=50 ack=1 ts=1us: acked[o2r]=100, snd_nxt=150, in_flight=50. ack=1 but
  r2o has no send -> no rtt. pending_rtt[o2r]=[(150,1us)]. => {o2r, in_flight 50, rtt null}
- seg2 r2o seq=1 ack=151 len=0 SACK[200,260) ts=2us: acked[r2o]=1, snd_nxt[r2o]=1, in_flight[r2o]=0.
  ack=151 advances acked[o2r] (100->151), pops (150,1us) -> rtt=2us-1us=1000ns (data RTT).
  sack_blocks non-empty -> sack=true. => {r2o, in_flight 0, rtt 1000, sack true}
