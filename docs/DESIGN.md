<!-- made by CasualArclamp -->
# DecAero design

A receiver for Inmarsat Classic Aero: every channel type, voice included, with a
"voice follow" mode that watches the voice band and starts a decoder when a new
carrier appears. The look is DecSAT's.

## Decisions (Rory, 2026-11-16)

| Question | Answer |
|---|---|
| Home | Its own repo, native egui window, DecSAT crates as git dependencies where they are light |
| Bands | L-band (P channel, ground half of C voice) and C-band (R, T, aircraft half of C voice) |
| Voice codec | AMBE 4800x3600 as a Rust port in the repo (`decaero-ambe`), patent notice in NOTICE |
| IQ in | SDR++ server, plain TCP/UDP IQ, files, and DecSAT's `decsat-server` compressed link |
| Test signals | A synthetic Aero modulator first; JAERO's sample recordings; the live feed when it exists |
| Outputs | Call recordings in Ogg Opus (small), a call log, JAERO-style ACARS / ADS-C feeds, aircraft lookup from a downloaded database, channel activity history |
| GitHub | Private `CasualArclamp/DecAero`, a push per milestone |
| Layout | Mockup 4 (three columns), with the left decoder panel carrying the constellation, a vertical margin bar, an Eb/N0 trend and Signal / FEC / Voice / Call sections. See `docs/mockups/index.html` |

## Channels

| Channel | Direction | Rate (bit/s) | Modulation | Coding | Frame |
|---|---|---|---|---|---|
| P | ground to aircraft, continuous | 600, 1200 | A-BPSK | rate 1/2 K=7, interleaver 64 x 6 or 9 | 32-bit unique word, 16-bit header, 1152 coded bits, 6 signal units |
| P | same | 10500 | A-QPSK | rate 1/2 K=7, interleaver 64 x 78 | unique word on both rails, header, 178 dummy bits, 4992 coded bits, 26 signal units |
| R | aircraft to ground, burst | 600, 1200, 10500 | A-BPSK / A-QPSK | rate 1/2, 64 x 5 | one 19-octet signal unit |
| T | aircraft to ground, burst | 600, 1200, 10500 | A-BPSK / A-QPSK | rate 1/2, 64 x (5 + 3n) | 6-octet header, n 12-octet signal units |
| C | both ways, per call | 8400 | A-QPSK | rate 2/3 (punctured K=7), 16 blocks of 64 x 4 | 52-bit unique word per rail, 25 voice frames of 96 bits, 3 sub-band signal units, every 0.5 s |
| C | both ways (legacy) | 21000 | A-QPSK | as the spec gives | 9.6 kbit/s voice: detected and named, not decoded |

Layouts are taken from JAERO (`aerol.cpp`), which is the open reference; the
Inmarsat SDM is not public. Constants cite the JAERO function they come from.

## Crates

- `decaero-ambe`: the voice decoder. Done.
- `decaero-core`: channel code, scrambler, interleaver, CRC (done); frame
  sync, frame codecs for P / R / T / C, signal units, ACARS reassembly (next).
- `decaero-dsp`: A-BPSK and A-QPSK demodulators built for low Eb/N0, the test
  modulator, the channeliser, the carrier watcher.
- `decaero-engine`: one front end thread fanning blocks out to a worker per
  channel (DecSAT's model), voice follow, recording, feeds, aircraft database.
- `apps/decaero-gui`, `apps/decaero-cli`.

## Low signal

The target is to hold lock to about 2 dB Eb/N0, where the K=7 code still
delivers. That needs: matched filtering at the exact symbol rate, a carrier
loop narrow enough for 600 bit/s (DecSAT's 250 Hz floor is far too wide),
unique-word correlation on soft symbols rather than a hard-bit compare,
frame-aided phase ambiguity resolution, and fly-wheeling over missed unique
words. Every claim is measured against the test modulator with added noise.

## Decoder figures shown (as DecSAT's DVB-S2 card)

Eb/N0 and C/N0, level, carrier offset and drift, clock error, phase scatter,
EVM / MER, **margin** over the decode limit, unique-word hits, **FEC load**
(share of the code's correcting power in use), pre-FEC BER, corrected bits,
CRC good / bad, voice frames repeated / muted, CPU.

## Voice follow

1. The P channel's C-channel assignment signal units name the frequency and
   the aircraft: a decoder is started there at once.
2. Independently, a carrier watcher on the voice band spectrum starts a
   decoder on any new carrier of the right width, so calls are followed when
   the P channel is not being received (and on C-band).
3. A decoder that loses its carrier for a few seconds is retired and the call
   is closed in the log.

## Milestones

0. Repo, codec port, channel code primitives. **Done.**
1. Frame codecs and the test modulator; P channel 600/1200 decoded from IQ.
2. A-QPSK: P 10500, C 8400 with voice out; check against JAERO's samples.
3. R / T bursts.
4. Engine, voice follow, Ogg Opus recording, call log.
5. GUI (three columns).
6. ACARS / ADS-C feeds, aircraft lookup, activity history.
7. Network inputs: SDR++ server, decsat-server; CI and portable exes.
