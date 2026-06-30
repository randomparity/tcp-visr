# Oracle derivations (hand-computed, RFC 1982 serial arithmetic)

These goldens are the **independent check** for M3 metric derivation (ADR-0007). Every
load-bearing value below is derived by hand from the fixture's segments — it is **not** a
snapshot of program output. If `tcp-visr metrics <fixture> --conn 0` disagrees with a value
here, the trace below is the authority and the code is the suspect. Regenerating a golden
(`tests/metrics.rs` is the byte-match check) requires re-deriving the numbers by hand and
reviewing the diff.

**Time units.** Fixture timestamps are in **microseconds**; the M1 replay faucet 0-bases each
capture to its first packet and reports **capture-relative nanoseconds**. So a fixture's
`1_000, 2_000, 3_000` µs become `t_ns = 0, 1_000_000, 2_000_000` (1 ms = 1_000_000 ns apart).
RTT and throughput below are in those capture-relative nanoseconds.

`o2r` = client(10.0.0.1:1234) -> server(10.0.0.2:80). `seq_end = seq + payload + SYN + FIN`.
in-flight = `serial_diff(snd_nxt[d], acked[d])` (clamped to 0 if acked is ahead). An ACK
advances (and can pair RTT) only once the acked direction has a tracked send. throughput_bps
= `8 * bytes_in_(t-window, t] * 1e9 / window_ns`; default window 1 s = 1_000_000_000 ns.

## seq_wrap.pcap (--conn 0) — t_ns = 0, 1_000_000, 2_000_000

- seg1 o2r seq=2^32-101 len=50 t=0: acked[o2r]=2^32-101, snd_nxt=2^32-51,
  in_flight = serial_diff(2^32-51, 2^32-101) = 50. tput: 50 B in window -> 8*50 = 400 bps.
  ACK=1 but r2o has no send -> rtt null. => {o2r, in_flight 50, tput 400, rtt null}
- seg2 o2r seq=200 len=50 t=1_000_000: snd_nxt=250 (forward wrap),
  in_flight=serial_diff(250,2^32-101)=351. tput: 50+50=100 B in (−1s,1ms] -> 800 bps.
  => {o2r, in_flight 351, tput 800, rtt null}
- seg3 r2o seq=1 ack=300 len=10 t=2_000_000: r2o in_flight=serial_diff(11,1)=10; tput: 10 B
  in r2o window -> 80 bps. ack=300 advances acked[o2r], covers o2r sends (2^32-51, 250 <= 300)
  -> rtt pairs oldest (t=0): rtt = 2_000_000 - 0 = 2_000_000 ns.
  => {r2o, in_flight 10, tput 80, rtt 2_000_000}

## metrics_basic.pcap (--conn 0) — t_ns = 0, 1_000_000, 2_000_000, 3_000_000

Exercises a piggybacked ACK (seg3 carries data AND acks the SYN-ACK); all four samples:

- seg1 o2r SYN seq=1000 t=0: acked[o2r]=1000, snd_nxt[o2r]=1001 (SYN phantom),
  in_flight=1. No ACK flag -> rtt null. pending_rtt[o2r]=[(1001,0)]. tput 0 (no payload).
  => {o2r, in_flight 1, tput 0, rtt null}
- seg2 r2o SYN-ACK seq=5000 ack=1001 t=1_000_000: acked[r2o]=5000, snd_nxt[r2o]=5001 (SYN
  phantom), in_flight[r2o]=1. ack=1001 advances acked[o2r] (1000->1001), pops (1001,0) ->
  rtt=1_000_000-0=1_000_000 (handshake RTT). pending_rtt[r2o]=[(5001,1_000_000)]. tput 0.
  => {r2o, in_flight 1, tput 0, rtt 1_000_000}
- seg3 o2r data seq=1001 len=100 ack=5001 t=2_000_000: snd_nxt[o2r]=1101, in_flight=100. Data
  -> pending_rtt[o2r]=[(1101,2_000_000)]. ack=5001 advances acked[r2o] (5000->5001), pops
  (5001,1_000_000) -> rtt=2_000_000-1_000_000=1_000_000 (SYN-ACK round trip). tput: 100 B in
  window -> 800 bps. => {o2r, in_flight 100, tput 800, rtt 1_000_000}
- seg4 r2o pure-ACK seq=5001 ack=1101 len=0 t=3_000_000: snd_nxt[r2o]=5001, acked[r2o] already
  5001 -> in_flight=0. ack=1101 advances acked[o2r] (1001->1101), pops (1101,2_000_000) ->
  rtt=3_000_000-2_000_000=1_000_000 (data RTT). tput 0.
  => {r2o, in_flight 0, tput 0, rtt 1_000_000}

So metrics_basic yields rtts [null, 1_000_000, 1_000_000, 1_000_000] and in_flight [1, 1, 100, 0].

## metrics_retransmit.pcap (--conn 0) — t_ns = 0, 3_000_000_000

Both segments are o2r; no reverse ACK, so no RTT anywhere.

- seg1 o2r seq=100 len=100 t=0: acked[o2r]=100, snd_nxt=200, in_flight=100, frontier=200.
  tput: 100 B -> 800 bps. => {o2r, in_flight 100, tput 800, rtt null}
- seg2 o2r seq=100 len=100 t=3_000_000_000 (3.0 s later): seq=100 < frontier 200, gap=3e9 ns
  >= reorder_window (3 ms = 3_000_000 ns) -> retransmit=true; clears pending RTT.
  in_flight=serial_diff(200,100)=100. tput: only seg2 in (2e9, 3e9] -> 800 bps.
  => {o2r, in_flight 100, tput 800, retransmit true}

## metrics_ooo.pcap (--conn 0) — t_ns = 0, 1000

Both o2r; no reverse ACK -> no rtt.

- seg1 o2r seq=200 len=100 t=0: in_flight=100, frontier=300. tput 800.
  => {o2r, in_flight 100, tput 800}
- seg2 o2r seq=100 len=100 t=1000 (1 µs later): seq=100 < frontier 300, gap=1000 ns < 3 ms ->
  out_of_order=true. in_flight=serial_diff(300,200)=100. tput: both segs in window (100+100) ->
  1600 bps. => {o2r, in_flight 100, tput 1600, out_of_order true}

## metrics_sack.pcap (--conn 0) — t_ns = 0, 1_000_000

seg2 also acks seg1, so it carries a data RTT.

- seg1 o2r seq=100 len=50 ack=1 t=0: acked[o2r]=100, snd_nxt=150, in_flight=50, tput 400.
  ack=1 but r2o has no send -> rtt null. pending_rtt[o2r]=[(150,0)].
  => {o2r, in_flight 50, tput 400, rtt null}
- seg2 r2o seq=1 ack=151 len=0 SACK[200,260) t=1_000_000: acked[r2o]=1, snd_nxt[r2o]=1,
  in_flight[r2o]=0. ack=151 advances acked[o2r] (100->151), pops (150,0) ->
  rtt=1_000_000-0=1_000_000 (data RTT). sack_blocks non-empty -> sack=true. tput 0.
  => {r2o, in_flight 0, tput 0, rtt 1_000_000, sack true}
